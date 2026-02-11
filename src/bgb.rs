use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::protocol::BgbPacket;

/// Thread-safe BGB client. Spawns a background thread that continuously
/// reads BGB packets and responds to sync/status. Data exchange happens
/// via channels so the caller never blocks on BGB directly.
pub struct BgbClient {
    /// Send a byte to exchange with BGB
    send_tx: mpsc::Sender<u8>,
    /// Receive the response byte from BGB
    recv_rx: mpsc::Receiver<u8>,
    /// Handle to the background thread
    _thread: std::thread::JoinHandle<()>,
}

impl BgbClient {
    pub fn connect(host: &str, port: u16, log_tx: Option<mpsc::Sender<String>>) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);
        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| format!("TCP connect to {}: {}", addr, e))?;
        stream.set_nodelay(true).ok();

        // Perform handshake on this thread before spawning
        let start_time = Instant::now();
        handshake(&mut stream, start_time)?;

        let (send_tx, send_rx) = mpsc::channel::<u8>();
        let (recv_tx, recv_rx) = mpsc::channel::<u8>();

        let thread = std::thread::spawn(move || {
            bgb_thread(stream, start_time, send_rx, recv_tx, log_tx);
        });

        Ok(Self {
            send_tx,
            recv_rx,
            _thread: thread,
        })
    }

    /// Exchange one byte with BGB. Sends the byte and waits for the response.
    /// Times out after 5 seconds.
    pub fn exchange_byte(&self, send: u8) -> Result<u8, String> {
        self.send_tx.send(send).map_err(|_| "BGB thread died".to_string())?;
        self.recv_rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "BGB exchange timeout".to_string())
    }
}

fn timestamp(start: Instant) -> u32 {
    let secs = start.elapsed().as_secs_f64();
    ((secs * (1u64 << 21) as f64) as u32) & 0x7FFF_FFFF
}

fn handshake(stream: &mut TcpStream, start: Instant) -> Result<(), String> {
    // Send version: protocol 1, max 4
    send_packet(stream, &BgbPacket::new(1, 1, 4, 0, 0))?;

    // Read version response
    let resp = read_packet(stream)?;
    if resp.command != 1 {
        return Err(format!("Expected version (cmd=1), got cmd={}", resp.command));
    }

    // Send initial status (running)
    send_packet(stream, &BgbPacket::new(108, 1, 0, 0, timestamp(start)))?;

    Ok(())
}

/// Background thread: continuously reads BGB packets, responds to sync/status,
/// and handles data exchange requests from the main thread.
fn bgb_thread(
    mut stream: TcpStream,
    start: Instant,
    send_rx: mpsc::Receiver<u8>,
    recv_tx: mpsc::Sender<u8>,
    log_tx: Option<mpsc::Sender<String>>,
) {
    // Use a short read timeout so we can check for pending sends
    stream.set_read_timeout(Some(Duration::from_millis(10))).ok();

    let mut waiting_for_response = false;

    loop {
        // Check if there's a byte to send (non-blocking)
        if !waiting_for_response {
            if let Ok(byte) = send_rx.try_recv() {
                let ts = timestamp(start);
                if send_packet(&mut stream, &BgbPacket::new(104, byte, 0x80, 0, ts)).is_err() {
                    return; // BGB disconnected
                }
                waiting_for_response = true;
            }
        }

        // Try to read a packet from BGB (with short timeout)
        match read_packet(&mut stream) {
            Ok(pkt) => {
                match pkt.command {
                    104 => {
                        // Sync1 from BGB — respond with sync2
                        let _ = send_packet(&mut stream, &BgbPacket::new(105, 0, 0x80, 0, pkt.timestamp));
                    }
                    105 => {
                        // Sync2 — data response from BGB
                        if waiting_for_response {
                            waiting_for_response = false;
                            if recv_tx.send(pkt.data).is_err() {
                                return; // Main thread dropped
                            }
                        }
                    }
                    106 => {
                        // Sync3 — echo it back
                        let _ = send_packet(&mut stream, &BgbPacket::new(106, pkt.data, pkt.extra1, pkt.extra2, pkt.timestamp));
                    }
                    108 => {
                        // Status — respond with running
                        let _ = send_packet(&mut stream, &BgbPacket::new(108, 1, 0, 0, pkt.timestamp));
                    }
                    109 => {
                        // Disconnect
                        if let Some(ref tx) = log_tx {
                            let _ = tx.send("BGB sent disconnect".into());
                        }
                        return;
                    }
                    _ => {}
                }
            }
            Err(ref e) if e.contains("timed out") || e.contains("WouldBlock") => {
                // No data available — this is normal, just loop
                // Small sleep to avoid busy-spinning when truly idle
                if !waiting_for_response {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(_) => {
                // Connection error
                return;
            }
        }
    }
}

fn send_packet(stream: &mut TcpStream, pkt: &BgbPacket) -> Result<(), String> {
    stream.write_all(&pkt.to_bytes()).map_err(|e| format!("BGB send: {}", e))
}

fn read_packet(stream: &mut TcpStream) -> Result<BgbPacket, String> {
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).map_err(|e| format!("{}", e))?;
    Ok(BgbPacket::from_bytes(buf))
}
