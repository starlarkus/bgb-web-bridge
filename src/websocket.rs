use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tungstenite::protocol::Message;
use tungstenite::accept;

use crate::bgb::BgbClient;
use crate::game::{GameThread, GameCommand, GameEvent};

/// Messages sent from the WebSocket thread back to the GUI.
pub enum WsEvent {
    Log(String),
    BrowserConnected,
    BrowserDisconnected,
    BgbConnected,
    BgbDisconnected,
    Stopped,
}

/// Messages sent from the GUI to the WebSocket thread.
pub enum WsCommand {
    Stop,
}

/// Run the WebSocket server. Blocks until stopped via command channel.
pub fn run(
    ws_port: u16,
    bgb_host: String,
    bgb_port: u16,
    event_tx: mpsc::Sender<WsEvent>,
    cmd_rx: mpsc::Receiver<WsCommand>,
    verbose: Arc<AtomicBool>,
) {
    let addr = format!("0.0.0.0:{}", ws_port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            let _ = event_tx.send(WsEvent::Log(format!("Failed to bind {}: {}", addr, e)));
            let _ = event_tx.send(WsEvent::Stopped);
            return;
        }
    };

    // Non-blocking so we can check for Stop commands
    listener.set_nonblocking(true).ok();

    let _ = event_tx.send(WsEvent::Log(format!("WebSocket server listening on {}", addr)));

    loop {
        // Check for stop command
        if let Ok(WsCommand::Stop) = cmd_rx.try_recv() {
            let _ = event_tx.send(WsEvent::Log("Stopping server...".into()));
            break;
        }

        // Try to accept a new connection
        let stream = match listener.accept() {
            Ok((stream, peer)) => {
                let _ = event_tx.send(WsEvent::Log(format!("Browser connected from {}", peer)));
                stream
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(e) => {
                let _ = event_tx.send(WsEvent::Log(format!("Accept error: {}", e)));
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };

        // Switch to blocking for the WebSocket connection
        stream.set_nonblocking(false).ok();

        let websocket = match accept(stream) {
            Ok(ws) => ws,
            Err(e) => {
                let _ = event_tx.send(WsEvent::Log(format!("WebSocket handshake failed: {}", e)));
                continue;
            }
        };

        let _ = event_tx.send(WsEvent::BrowserConnected);

        handle_connection(websocket, &bgb_host, bgb_port, &event_tx, &cmd_rx, &verbose);

        let _ = event_tx.send(WsEvent::BrowserDisconnected);
    }

    let _ = event_tx.send(WsEvent::Stopped);
}

fn handle_connection(
    mut websocket: tungstenite::WebSocket<std::net::TcpStream>,
    bgb_host: &str,
    bgb_port: u16,
    event_tx: &mpsc::Sender<WsEvent>,
    cmd_rx: &mpsc::Receiver<WsCommand>,
    verbose: &Arc<AtomicBool>,
) {
    // Create a log sender that forwards BGB thread logs to the GUI
    let bgb_log_tx = {
        let tx = event_tx.clone();
        let (log_tx, log_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            while let Ok(msg) = log_rx.recv() {
                let _ = tx.send(WsEvent::Log(msg));
            }
        });
        log_tx
    };

    // Connect to BGB
    let bgb = match BgbClient::connect(bgb_host, bgb_port, Some(bgb_log_tx), verbose.clone()) {
        Ok(b) => {
            let _ = event_tx.send(WsEvent::BgbConnected);
            let _ = event_tx.send(WsEvent::Log("Connected to BGB".into()));
            b
        }
        Err(e) => {
            let _ = event_tx.send(WsEvent::Log(format!("BGB connect failed: {}", e)));
            let _ = event_tx.send(WsEvent::BgbDisconnected);
            let _ = websocket.close(None);
            return;
        }
    };

    // Create channels for game thread communication
    let (game_cmd_tx, game_cmd_rx) = mpsc::channel::<GameCommand>();
    let (game_event_tx, game_event_rx) = mpsc::channel::<GameEvent>();

    // Spawn the game thread
    let game_thread = std::thread::spawn(move || {
        let mut game = GameThread::new(bgb, game_cmd_rx, game_event_tx);
        game.run();
    });

    // Set a read timeout so we can periodically check for stop commands and game events
    let _ = websocket.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(50)));

    loop {
        // Check for stop command from GUI
        if let Ok(WsCommand::Stop) = cmd_rx.try_recv() {
            let _ = game_cmd_tx.send(GameCommand::Stop);
            let _ = websocket.close(None);
            break;
        }

        // Forward game events to browser as JSON
        while let Ok(event) = game_event_rx.try_recv() {
            match &event {
                GameEvent::Log(msg) => {
                    let _ = event_tx.send(WsEvent::Log(msg.clone()));
                }
                _ => {
                    let json = game_event_to_json(&event);
                    if let Err(e) = websocket.write(Message::Text(json)) {
                        let _ = event_tx.send(WsEvent::Log(format!("WebSocket write error: {}", e)));
                        let _ = game_cmd_tx.send(GameCommand::Stop);
                        break;
                    }
                    let _ = websocket.flush();
                }
            }
        }

        // Read WebSocket messages from browser
        let msg = match websocket.read() {
            Ok(msg) => msg,
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => {
                let _ = event_tx.send(WsEvent::Log(format!("WebSocket read error: {}", e)));
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                if let Some(cmd) = parse_browser_command(&text) {
                    if game_cmd_tx.send(cmd).is_err() {
                        let _ = event_tx.send(WsEvent::Log("Game thread died".into()));
                        break;
                    }
                } else {
                    let _ = event_tx.send(WsEvent::Log(format!("Unknown command: {}", text)));
                }
            }
            Message::Close(_) => {
                let _ = event_tx.send(WsEvent::Log("Browser disconnected".into()));
                break;
            }
            _ => {
                // Ignore binary, ping, pong
            }
        }
    }

    // Clean up game thread
    let _ = game_cmd_tx.send(GameCommand::Stop);
    let _ = game_thread.join();

    let _ = event_tx.send(WsEvent::BgbDisconnected);
}

// ── JSON message handling ──────────────────────────────────────────────

fn game_event_to_json(event: &GameEvent) -> String {
    match event {
        GameEvent::Connected => r#"{"event":"connected"}"#.to_string(),
        GameEvent::Height(v) => format!(r#"{{"event":"height","value":{}}}"#, v),
        GameEvent::Lines(v) => format!(r#"{{"event":"lines","value":{}}}"#, v),
        GameEvent::Win => r#"{"event":"win"}"#.to_string(),
        GameEvent::Lose => r#"{"event":"lose"}"#.to_string(),
        GameEvent::ScreenFilled => r#"{"event":"screen_filled"}"#.to_string(),
        GameEvent::Log(_) => unreachable!(), // handled separately
    }
}

fn parse_browser_command(text: &str) -> Option<GameCommand> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let cmd = json.get("cmd")?.as_str()?;

    match cmd {
        "set_game" => {
            let game = json.get("game")?.as_str()?.to_string();
            Some(GameCommand::SetGame(game))
        }
        "set_music" => {
            let music = json.get("music")?.as_u64()? as u8;
            Some(GameCommand::SetMusic(music))
        }
        "confirm_music" => Some(GameCommand::ConfirmMusic),
        "start_game" => {
            let garbage = json.get("garbage")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u8))
                .collect();
            let tiles = json.get("tiles")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as u8))
                .collect();
            let is_first = json.get("is_first").and_then(|v| v.as_bool()).unwrap_or(true);
            Some(GameCommand::StartGame { garbage, tiles, is_first })
        }
        "set_height" => {
            let value = json.get("value")?.as_u64()? as u8;
            Some(GameCommand::SetHeight(value))
        }
        "queue_command" => {
            let value = json.get("value")?.as_u64()? as u8;
            Some(GameCommand::QueueCommand(value))
        }
        _ => None,
    }
}
