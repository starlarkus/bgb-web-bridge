#![windows_subsystem = "windows"]

mod bgb;
mod bridge;
mod protocol;
mod websocket;

use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::io::Write as IoWrite;
use eframe::egui;
use websocket::{WsCommand, WsEvent};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([400.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GB Bridge - BGB Emulator",
        options,
        Box::new(|_cc| Ok(Box::new(BridgeApp::default()))),
    )
}

struct BridgeApp {
    bgb_port: String,
    ws_port: String,
    running: bool,
    verbose: bool,
    bgb_connected: bool,
    browser_connected: bool,
    log: Vec<String>,
    cmd_tx: Option<mpsc::Sender<WsCommand>>,
    event_rx: Option<mpsc::Receiver<WsEvent>>,
    verbose_flag: Option<Arc<AtomicBool>>,
    log_file: Option<std::io::BufWriter<std::fs::File>>,
    start_instant: Option<std::time::Instant>,
}

impl Default for BridgeApp {
    fn default() -> Self {
        Self {
            bgb_port: "8765".into(),
            ws_port: "8767".into(),
            running: false,
            verbose: false,
            bgb_connected: false,
            browser_connected: false,
            log: vec!["Ready. Configure ports and click Start.".into()],
            cmd_tx: None,
            event_rx: None,
            verbose_flag: None,
            log_file: None,
            start_instant: None,
        }
    }
}

impl BridgeApp {
    fn start(&mut self) {
        let ws_port: u16 = match self.ws_port.parse() {
            Ok(p) => p,
            Err(_) => { self.log.push("Invalid WebSocket port".into()); return; }
        };
        let bgb_port: u16 = match self.bgb_port.parse() {
            Ok(p) => p,
            Err(_) => { self.log.push("Invalid BGB port".into()); return; }
        };

        let (event_tx, event_rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();

        let verbose_flag = Arc::new(AtomicBool::new(self.verbose));
        self.verbose_flag = Some(verbose_flag.clone());

        // Open log file
        let start_instant = std::time::Instant::now();
        self.start_instant = Some(start_instant);
        match std::fs::File::create("bgb-bridge.log") {
            Ok(f) => {
                let mut writer = std::io::BufWriter::new(f);
                let _ = writeln!(writer, "=== BGB Bridge Log ===");
                self.log_file = Some(writer);
            }
            Err(e) => {
                self.log.push(format!("Warning: could not create log file: {}", e));
            }
        }

        self.event_rx = Some(event_rx);
        self.cmd_tx = Some(cmd_tx);
        self.running = true;
        self.bgb_connected = false;
        self.browser_connected = false;
        self.log.push(format!("Starting... WS:{} BGB:{}", ws_port, bgb_port));
        self.write_log("Starting bridge");

        let bgb_host = "127.0.0.1".to_string();
        std::thread::spawn(move || {
            websocket::run(ws_port, bgb_host, bgb_port, event_tx, cmd_rx, verbose_flag);
        });
    }

    fn stop(&mut self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(WsCommand::Stop);
        }
        self.log.push("Stop requested...".into());
        self.write_log("Stop requested");
        // Flush and close log file
        if let Some(ref mut f) = self.log_file {
            let _ = f.flush();
        }
    }

    fn write_log(&mut self, msg: &str) {
        if let (Some(ref mut f), Some(start)) = (&mut self.log_file, self.start_instant) {
            let elapsed = start.elapsed();
            let secs = elapsed.as_secs();
            let millis = elapsed.subsec_millis();
            let _ = writeln!(f, "[{:02}:{:02}:{:02}.{:03}] {}",
                secs / 3600, (secs % 3600) / 60, secs % 60, millis, msg);
        }
    }

    fn poll_events(&mut self) {
        if let Some(rx) = &self.event_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    WsEvent::Log(msg) => {
                        self.write_log(&msg);
                        self.log.push(msg);
                        // Cap log to prevent unbounded memory growth
                        if self.log.len() > 500 {
                            self.log.drain(..self.log.len() - 300);
                        }
                    }
                    WsEvent::BrowserConnected => self.browser_connected = true,
                    WsEvent::BrowserDisconnected => self.browser_connected = false,
                    WsEvent::BgbConnected => self.bgb_connected = true,
                    WsEvent::BgbDisconnected => self.bgb_connected = false,
                    WsEvent::Stopped => {
                        self.running = false;
                        self.bgb_connected = false;
                        self.browser_connected = false;
                        self.cmd_tx = None;
                        self.event_rx = None;
                        self.log.push("Stopped.".into());
                        self.write_log("Stopped");
                        if let Some(ref mut f) = self.log_file {
                            let _ = f.flush();
                        }
                        self.verbose_flag = None;
                        return;
                    }
                }
            }
        }
        // Periodically flush log file
        if let Some(ref mut f) = self.log_file {
            let _ = f.flush();
        }
    }
}

impl eframe::App for BridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();

        // Request repaint periodically to pick up events from the bridge thread
        if self.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("GB Bridge - BGB Emulator");
            ui.add_space(8.0);

            // Port configuration
            ui.horizontal(|ui| {
                ui.label("BGB Port:");
                ui.add_enabled(!self.running, egui::TextEdit::singleline(&mut self.bgb_port).desired_width(60.0));
                ui.add_space(16.0);
                ui.label("WS Port:");
                ui.add_enabled(!self.running, egui::TextEdit::singleline(&mut self.ws_port).desired_width(60.0));
            });

            ui.add_space(8.0);

            // Start/Stop and Verbose
            ui.horizontal(|ui| {
                if self.running {
                    if ui.button("Stop").clicked() {
                        self.stop();
                    }
                } else if ui.button("Start").clicked() {
                    self.start();
                }

                ui.add_space(16.0);

                if ui.checkbox(&mut self.verbose, "Verbose Logs").changed() {
                    if let Some(ref flag) = self.verbose_flag {
                        flag.store(self.verbose, Ordering::Relaxed);
                    }
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(4.0);

            // Status indicators
            ui.horizontal(|ui| {
                ui.label("BGB:");
                if self.bgb_connected {
                    ui.colored_label(egui::Color32::GREEN, "Connected");
                } else {
                    ui.colored_label(egui::Color32::GRAY, "Disconnected");
                }
                ui.add_space(24.0);
                ui.label("Browser:");
                if self.browser_connected {
                    ui.colored_label(egui::Color32::GREEN, "Connected");
                } else {
                    ui.colored_label(egui::Color32::GRAY, "Disconnected");
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // Log area
            ui.label("Log:");
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(280.0)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.label(line);
                    }
                });
        });
    }
}
