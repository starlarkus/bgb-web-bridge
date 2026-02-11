use std::net::TcpListener;
use std::sync::mpsc;

use tungstenite::protocol::Message;
use tungstenite::accept;

use crate::bridge::Bridge;

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
/// Accepts one browser connection at a time; for each incoming binary message,
/// calls the bridge handler and sends the response back.
pub fn run(
    ws_port: u16,
    bgb_host: String,
    bgb_port: u16,
    event_tx: mpsc::Sender<WsEvent>,
    cmd_rx: mpsc::Receiver<WsCommand>,
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

        handle_connection(websocket, &bgb_host, bgb_port, &event_tx, &cmd_rx);

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
    let bridge = match Bridge::new(bgb_host, bgb_port, Some(bgb_log_tx)) {
        Ok(b) => {
            let _ = event_tx.send(WsEvent::BgbConnected);
            let _ = event_tx.send(WsEvent::Log("Connected to BGB".into()));
            b
        }
        Err(e) => {
            let _ = event_tx.send(WsEvent::Log(format!("BGB connect failed: {}", e)));
            let _ = event_tx.send(WsEvent::BgbDisconnected);
            // Send error to browser and close
            let _ = websocket.close(None);
            return;
        }
    };

    // Set a read timeout so we can periodically check for stop commands
    if let Ok(peer) = websocket.get_ref().peer_addr() {
        let _ = websocket.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(100)));
        let _ = peer; // suppress warning
    }

    loop {
        // Check for stop command
        if let Ok(WsCommand::Stop) = cmd_rx.try_recv() {
            let _ = websocket.close(None);
            break;
        }

        let msg = match websocket.read() {
            Ok(msg) => msg,
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                continue;
            }
            Err(e) => {
                let _ = event_tx.send(WsEvent::Log(format!("WebSocket read error: {}", e)));
                break;
            }
        };

        match msg {
            Message::Binary(data) => {
                match bridge.handle_message(&data) {
                    Ok(response) => {
                        if let Err(e) = websocket.write(Message::Binary(response.into())) {
                            let _ = event_tx.send(WsEvent::Log(format!("WebSocket write error: {}", e)));
                            break;
                        }
                        let _ = websocket.flush();
                    }
                    Err(e) => {
                        let _ = event_tx.send(WsEvent::Log(format!("Bridge error: {}", e)));
                        break;
                    }
                }
            }
            Message::Close(_) => {
                let _ = event_tx.send(WsEvent::Log("Browser disconnected".into()));
                break;
            }
            _ => {
                // Ignore text, ping, pong — browser sends binary
            }
        }
    }

    let _ = event_tx.send(WsEvent::BgbDisconnected);
}
