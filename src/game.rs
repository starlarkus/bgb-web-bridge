use std::sync::mpsc;
use std::time::Duration;
use std::thread;

use crate::bgb::BgbClient;

// ── Messages between WebSocket thread and game thread ──────────────────

/// Commands sent from the WebSocket thread to the game thread.
#[derive(Debug)]
pub enum GameCommand {
    /// Tell the bridge what game to play (e.g. "tetris")
    SetGame(String),
    /// Set the music selection byte
    SetMusic(u8),
    /// Confirm music selection (transitions to waiting for start_game)
    ConfirmMusic,
    /// Start a game with initial garbage lines and tile data
    StartGame {
        garbage: Vec<u8>,
        tiles: Vec<u8>,
        is_first: bool,
    },
    /// Update opponent height to send to the Game Boy
    SetHeight(u8),
    /// Queue a win/lose/lines command to send to the Game Boy
    QueueCommand(u8),
    /// Stop the game thread
    Stop,
}

/// Events sent from the game thread to the WebSocket thread.
#[derive(Debug, Clone)]
pub enum GameEvent {
    /// BGB probe succeeded (0x29→0x55), ready for music
    Connected,
    /// Height value read from the Game Boy
    Height(u8),
    /// Lines signal from the Game Boy (0x80..0x85)
    Lines(u8),
    /// Game Boy reports 30 lines reached (0x77) — we win
    Win,
    /// Game Boy reports topped out (0xAA) — we lose
    Lose,
    /// Game Boy reports screen filled after loss (0xFF)
    ScreenFilled,
    /// Log message
    Log(String),
}

// ── Game phases ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    /// Waiting for the browser to send set_game
    WaitingForGame,
    /// Probing the Game Boy (sending 0x29, expecting 0x55)
    Probing,
    /// Sending music selection bytes periodically
    MusicSelect,
    /// Music confirmed, waiting for start_game command
    WaitingForStart,
    /// Sending the game start byte sequence
    GameStarting,
    /// Game loop: exchanging height/lines/win/lose every ~100ms
    InGame,
}

// ── Game thread ────────────────────────────────────────────────────────

pub struct GameThread {
    bgb: BgbClient,
    cmd_rx: mpsc::Receiver<GameCommand>,
    event_tx: mpsc::Sender<GameEvent>,
    phase: Phase,
    music_byte: u8,
    opponent_height: u8,
    command_queue: Vec<u8>,
    game_started_at: Option<std::time::Instant>,
}

impl GameThread {
    pub fn new(
        bgb: BgbClient,
        cmd_rx: mpsc::Receiver<GameCommand>,
        event_tx: mpsc::Sender<GameEvent>,
    ) -> Self {
        Self {
            bgb,
            cmd_rx,
            event_tx,
            phase: Phase::WaitingForGame,
            music_byte: 0x1C, // default: A-Type music
            opponent_height: 0,
            command_queue: Vec::new(),
            game_started_at: None,
        }
    }

    /// Run the game thread. Blocks until stopped or BGB disconnects.
    pub fn run(&mut self) {
        self.log("Game thread started");

        loop {
            // Check for commands (non-blocking) — returns true if we should stop
            if self.process_commands() {
                return;
            }

            // Run the current phase
            match self.phase {
                Phase::WaitingForGame => {
                    thread::sleep(Duration::from_millis(50));
                }
                Phase::Probing => {
                    self.run_probe();
                }
                Phase::MusicSelect => {
                    self.run_music_exchange();
                    thread::sleep(Duration::from_millis(100));
                }
                Phase::WaitingForStart => {
                    thread::sleep(Duration::from_millis(50));
                }
                Phase::GameStarting => {
                    // Handled by start_game command processing
                    thread::sleep(Duration::from_millis(50));
                }
                Phase::InGame => {
                    self.run_game_loop_tick();
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Process pending commands. Returns true if the game thread should stop.
    fn process_commands(&mut self) -> bool {
        // Drain all pending commands
        loop {
            match self.cmd_rx.try_recv() {
                Ok(cmd) => match cmd {
                    GameCommand::SetGame(game) => {
                        self.log(&format!("Game set to: {}", game));
                        self.phase = Phase::Probing;
                    }
                    GameCommand::SetMusic(byte) => {
                        self.music_byte = byte;
                    }
                    GameCommand::ConfirmMusic => {
                        self.log("Music confirmed");
                        // Send 0x50 to confirm music selection
                        let _ = self.exchange(0x50);
                        self.phase = Phase::WaitingForStart;
                    }
                    GameCommand::StartGame { garbage, tiles, is_first } => {
                        self.log(&format!("Starting game (first={}, garbage={}, tiles={})",
                            is_first, garbage.len(), tiles.len()));
                        self.run_game_start_sequence(&garbage, &tiles, is_first);
                    }
                    GameCommand::SetHeight(h) => {
                        self.opponent_height = h;
                    }
                    GameCommand::QueueCommand(cmd) => {
                        self.command_queue.push(cmd);
                    }
                    GameCommand::Stop => {
                        self.log("Game thread stopping");
                        return true;
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.log("WebSocket thread disconnected, stopping game thread");
                    return true;
                }
            }
        }
        false
    }

    // ── Phase implementations ──────────────────────────────────────────

    fn run_probe(&mut self) {
        self.log("Probing Game Boy...");
        match self.exchange(0x29) {
            Ok(response) => {
                if response == 0x55 {
                    self.log("Probe OK (0x55)");
                    self.send_event(GameEvent::Connected);
                    self.phase = Phase::MusicSelect;
                } else {
                    self.log(&format!("Probe unexpected: 0x{:02X}, retrying...", response));
                    thread::sleep(Duration::from_millis(500));
                }
            }
            Err(e) => {
                self.log(&format!("Probe failed: {}", e));
                thread::sleep(Duration::from_millis(1000));
            }
        }
    }

    fn run_music_exchange(&mut self) {
        // Send the music byte and ignore the response (like the music timer)
        let _ = self.exchange(self.music_byte);
    }

    fn run_game_start_sequence(&mut self, garbage: &[u8], tiles: &[u8], is_first: bool) {
        self.phase = Phase::GameStarting;
        self.command_queue.clear();
        self.opponent_height = 0;

        if is_first {
            self.log("First game start sequence");
            // Step 1: start game message
            self.exchange_n(0x60, 150);
            self.exchange_n(0x29, 4);
        } else {
            self.log("Subsequent game start sequence");
            // Begin communication again
            self.exchange_n(0x60, 70);
            self.exchange_n(0x02, 70);
            self.exchange_n(0x02, 70);
            self.exchange_n(0x02, 70);
            self.exchange_n(0x79, 330);
            // Send start
            self.exchange_n(0x60, 150);
            self.exchange_n(0x29, 70);
        }

        // Step 3: send initial garbage
        self.log(&format!("Sending {} garbage bytes", garbage.len()));
        for &g in garbage {
            self.exchange_n(g, 4);
        }

        // Step 4: send master again
        self.exchange_n(0x29, 8);

        // Step 5: send tiles
        self.log(&format!("Sending {} tile bytes", tiles.len()));
        for &t in tiles {
            self.exchange_n(t, 4);
        }

        // Step 6: and go
        self.exchange_n(0x30, 70);
        self.exchange_n(0x00, 70);
        self.exchange_n(0x02, 70);
        self.exchange_n(0x02, 70);
        self.exchange_n(0x20, 70);

        self.log("Game start sequence complete, entering game loop");
        self.game_started_at = Some(std::time::Instant::now());
        self.phase = Phase::InGame;
    }

    fn run_game_loop_tick(&mut self) {
        // Determine what byte to send (priority: queued commands > opponent height)
        let byte_to_send = if !self.command_queue.is_empty() {
            self.command_queue.remove(0)
        } else {
            self.opponent_height
        };

        match self.exchange(byte_to_send) {
            Ok(value) => {
                self.interpret_game_byte(value);
            }
            Err(e) => {
                self.log(&format!("Game loop exchange error: {}", e));
            }
        }
    }

    fn interpret_game_byte(&mut self, value: u8) {
        if value < 20 {
            // Height value
            self.send_event(GameEvent::Height(value));
        } else if value >= 0x80 && value <= 0x85 {
            // Lines sent
            self.send_event(GameEvent::Lines(value));
        } else if value == 0x77 {
            // We won by reaching 30 lines
            self.log("Game Boy reports WIN (0x77)");
            self.send_event(GameEvent::Win);
        } else if value == 0xAA {
            // We lost (topped out)
            // Ignore topped-out signal in first 3 seconds
            if let Some(started) = self.game_started_at {
                if started.elapsed().as_millis() < 3000 {
                    self.log("Ignoring topped out - game just started");
                    return;
                }
            }
            self.log("Game Boy reports LOSE (0xAA)");
            self.send_event(GameEvent::Lose);
        } else if value == 0xFF {
            // Screen filled after loss
            self.send_event(GameEvent::ScreenFilled);
            // Queue the final screen command
            self.command_queue.push(0x43);
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    /// Exchange one byte with BGB via the link cable.
    fn exchange(&self, byte: u8) -> Result<u8, String> {
        self.bgb.exchange_byte(byte)
    }

    /// Exchange one byte, then sleep for `delay_ms`. Used for timed sequences.
    fn exchange_n(&self, byte: u8, delay_ms: u64) {
        let _ = self.exchange(byte);
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
    }

    fn send_event(&self, event: GameEvent) {
        let _ = self.event_tx.send(event);
    }

    fn log(&self, msg: &str) {
        let _ = self.event_tx.send(GameEvent::Log(msg.to_string()));
    }
}
