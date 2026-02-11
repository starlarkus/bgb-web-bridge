use std::io::{self, Read, Write};
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
    let resp = read_packet(stream).map_err(|e| format!("BGB handshake read: {}", e))?;
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
    // Non-blocking mode — we manually poll with short sleeps
    stream.set_nonblocking(true).ok();

    let log = |msg: String| {
        if let Some(ref tx) = log_tx {
            let _ = tx.send(msg);
        }
    };

    let mut waiting_for_response = false;
    let mut read_buf = [0u8; 64];
    let mut read_pos: usize = 0;

    loop {
        // Check if there's a byte to send (non-blocking)
        if !waiting_for_response {
            match send_rx.try_recv() {
                Ok(byte) => {
                    let ts = timestamp(start);
                    if send_packet(&mut stream, &BgbPacket::new(104, byte, 0x80, 0, ts)).is_err() {
                        log("BGB send failed, disconnecting".into());
                        return;
                    }
                    waiting_for_response = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log("Bridge dropped, closing BGB connection".into());
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Read available bytes into packet buffer (non-blocking, no desync risk)
        match stream.read(&mut read_buf[read_pos..]) {
            Ok(0) => {
                log("BGB connection closed".into());
                return;
            }
            Ok(n) => {
                read_pos += n;
            }
            Err(ref e) if is_timeout(e) => {
                // No data available right now
                if read_pos == 0 && !waiting_for_response {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(e) => {
                log(format!("BGB connection lost: {}", e));
                return;
            }
        }

        // Process complete packets
        while read_pos >= 8 {
            let pkt = BgbPacket::from_bytes([
                read_buf[0], read_buf[1], read_buf[2], read_buf[3],
                read_buf[4], read_buf[5], read_buf[6], read_buf[7],
            ]);

            // Shift remaining bytes to front
            let remaining = read_pos - 8;
            if remaining > 0 {
                read_buf.copy_within(8.., 0);
            }
            read_pos = remaining;

            match pkt.command {
                104 => {
                    let _ = send_packet(&mut stream, &BgbPacket::new(105, 0, 0x80, 0, pkt.timestamp));
                }
                105 => {
                    if waiting_for_response {
                        waiting_for_response = false;
                        if recv_tx.send(pkt.data).is_err() {
                            return;
                        }
                    }
                }
                106 => {
                    let _ = send_packet(&mut stream, &BgbPacket::new(106, pkt.data, pkt.extra1, pkt.extra2, pkt.timestamp));
                }
                108 => {
                    let _ = send_packet(&mut stream, &BgbPacket::new(108, 1, 0, 0, pkt.timestamp));
                }
                109 => {
                    log("BGB sent disconnect".into());
                    return;
                }
                _ => {}
            }
        }
    }
}

/// Check if an IO error is a timeout/would-block (cross-platform).
fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

fn send_packet(stream: &mut TcpStream, pkt: &BgbPacket) -> Result<(), String> {
    stream.write_all(&pkt.to_bytes()).map_err(|e| format!("BGB send: {}", e))
}

fn read_packet(stream: &mut TcpStream) -> Result<BgbPacket, io::Error> {
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf)?;
    Ok(BgbPacket::from_bytes(buf))
}
