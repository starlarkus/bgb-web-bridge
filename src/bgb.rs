use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
// Note: Instant used only for verbose logging (last_exchange_time), not for BGB timestamps.

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
    pub fn connect(host: &str, port: u16, log_tx: Option<mpsc::Sender<String>>, verbose: Arc<AtomicBool>) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);
        let mut stream = TcpStream::connect(&addr)
            .map_err(|e| format!("TCP connect to {}: {}", addr, e))?;
        stream.set_nodelay(true).ok();

        // Perform handshake on this thread before spawning
        handshake(&mut stream)?;

        let (send_tx, send_rx) = mpsc::channel::<u8>();
        let (recv_tx, recv_rx) = mpsc::channel::<u8>();

        let thread = std::thread::spawn(move || {
            bgb_thread(stream, send_rx, recv_tx, log_tx, verbose);
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

fn handshake(stream: &mut TcpStream) -> Result<(), String> {
    // Send version: protocol 1, max 4
    send_packet(stream, &BgbPacket::new(1, 1, 4, 0, 0))?;

    // Read version response
    let resp = read_packet(stream).map_err(|e| format!("BGB handshake read: {}", e))?;
    if resp.command != 1 {
        return Err(format!("Expected version (cmd=1), got cmd={}", resp.command));
    }

    // Send initial status (running) — timestamp 0, BGB will tell us its clock
    send_packet(stream, &BgbPacket::new(108, 1, 0, 0, 0))?;

    Ok(())
}

/// Background thread: continuously reads BGB packets, responds to sync/status,
/// and handles data exchange requests from the main thread.
fn bgb_thread(
    mut stream: TcpStream,
    send_rx: mpsc::Receiver<u8>,
    recv_tx: mpsc::Sender<u8>,
    log_tx: Option<mpsc::Sender<String>>,
    verbose: Arc<AtomicBool>,
) {
    // Non-blocking mode — we manually poll with short sleeps
    stream.set_nonblocking(true).ok();

    let log = |msg: String| {
        if let Some(ref tx) = log_tx {
            let _ = tx.send(msg);
        }
    };

    let vlog = |msg: String| {
        if verbose.load(Ordering::Relaxed) {
            if let Some(ref tx) = log_tx {
                let _ = tx.send(msg);
            }
        }
    };

    let mut waiting_for_response = false;
    let mut pending_byte: u8 = 0; // The byte we sent in our last cmd=104
    let mut read_buf = [0u8; 64];
    let mut read_pos: usize = 0;
    let mut exchange_count: u64 = 0;
    let mut last_exchange_time = Instant::now();
    // Simple monotonic counter for BGB timestamps. We always send timestamps
    // "in the past" relative to BGB's real clock, so BGB processes transfers
    // immediately without needing to emulate forward.
    let mut next_timestamp: u32 = 0;

    loop {
        // Check if there's a byte to send (non-blocking)
        if !waiting_for_response {
            match send_rx.try_recv() {
                Ok(byte) => {
                    next_timestamp = next_timestamp.wrapping_add(8192);
                    let ts = next_timestamp;
                    // SC=0x81: internal clock. We (web client) are the clock master,
                    // the Game Boy in BGB is the slave.
                    if send_packet(&mut stream, &BgbPacket::new(104, byte, 0x81, 0, ts)).is_err() {
                        log("BGB send failed, disconnecting".into());
                        return;
                    }
                    pending_byte = byte;
                    waiting_for_response = true;
                    exchange_count += 1;
                    last_exchange_time = Instant::now();
                    vlog(format!("[SEND] sync1 #{}: data=0x{:02X} sc=0x81 ts={}", exchange_count, byte, ts));
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
                // No data available right now — log if we've been waiting a while
                if waiting_for_response {
                    let waited = last_exchange_time.elapsed();
                    if waited.as_secs() >= 2 && waited.as_secs() % 2 == 0 && waited.subsec_millis() < 5 {
                        vlog(format!("[WAIT] sync2 for #{} (sent 0x{:02X}): waiting {}s...",
                            exchange_count, pending_byte, waited.as_secs()));
                    }
                }
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
                    if waiting_for_response {
                        // Simultaneous exchange: both sides sent sync1.
                        // Respond with our pending byte and treat BGB's data as our response.
                        let elapsed_ms = last_exchange_time.elapsed().as_millis();
                        let _ = send_packet(&mut stream, &BgbPacket::new(105, pending_byte, 0x80, 0, pkt.timestamp));
                        waiting_for_response = false;
                        vlog(format!("[RECV] sync1 #{} (SIMUL): bgb_data=0x{:02X} sc=0x{:02X} -> reply 0x{:02X} ({}ms)",
                            exchange_count, pkt.data, pkt.extra1, pending_byte, elapsed_ms));
                        if recv_tx.send(pkt.data).is_err() {
                            return;
                        }
                    } else {
                        // BGB initiated a transfer while we have nothing to send
                        let _ = send_packet(&mut stream, &BgbPacket::new(105, 0, 0x80, 0, pkt.timestamp));
                        vlog(format!("[RECV] sync1 (unsolicited): bgb_data=0x{:02X} sc=0x{:02X} -> reply 0x00",
                            pkt.data, pkt.extra1));
                    }
                }
                105 => {
                    if waiting_for_response {
                        let elapsed_ms = last_exchange_time.elapsed().as_millis();
                        waiting_for_response = false;
                        vlog(format!("[RECV] sync2 #{}: data=0x{:02X} sc=0x{:02X} ({}ms)",
                            exchange_count, pkt.data, pkt.extra1, elapsed_ms));
                        if recv_tx.send(pkt.data).is_err() {
                            return;
                        }
                    } else {
                        vlog(format!("[RECV] sync2 (stale): data=0x{:02X} sc=0x{:02X} — ignoring",
                            pkt.data, pkt.extra1));
                    }
                }
                106 => {
                    let _ = send_packet(&mut stream, &BgbPacket::new(106, pkt.data, pkt.extra1, pkt.extra2, pkt.timestamp));
                    vlog(format!("[RECV] sync3: data=0x{:02X}", pkt.data));
                }
                108 => {
                    let _ = send_packet(&mut stream, &BgbPacket::new(108, 1, 0, 0, pkt.timestamp));
                    vlog(format!("[RECV] status: data=0x{:02X} extra1=0x{:02X}", pkt.data, pkt.extra1));
                }
                109 => {
                    log("BGB sent disconnect".into());
                    return;
                }
                _ => {
                    vlog(format!("[RECV] unknown cmd={}: data=0x{:02X} extra1=0x{:02X} extra2=0x{:02X}",
                        pkt.command, pkt.data, pkt.extra1, pkt.extra2));
                }
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
