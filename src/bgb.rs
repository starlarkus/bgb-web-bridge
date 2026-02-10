use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

use crate::protocol::BgbPacket;

pub struct BgbClient {
    stream: TcpStream,
    start_time: Instant,
}

impl BgbClient {
    /// Connect to BGB and perform the version handshake.
    pub fn connect(host: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).map_err(|e| format!("TCP connect to {}: {}", addr, e))?;
        stream.set_nodelay(true).ok();

        let mut client = Self {
            stream,
            start_time: Instant::now(),
        };
        client.handshake()?;
        Ok(client)
    }

    /// Timestamp matching BGB protocol: real time * 2^21, masked to 31 bits.
    fn timestamp(&self) -> u32 {
        let secs = self.start_time.elapsed().as_secs_f64();
        ((secs * (1u64 << 21) as f64) as u32) & 0x7FFF_FFFF
    }

    fn handshake(&mut self) -> Result<(), String> {
        // Send version packet: protocol version 1, max protocol 4
        let pkt = BgbPacket::new(1, 1, 4, 0, 0);
        self.send_packet(&pkt)?;

        // Read version response
        let resp = self.read_packet()?;
        if resp.command != 1 {
            return Err(format!("Expected version response (cmd=1), got cmd={}", resp.command));
        }

        // Send initial status (running)
        let status = BgbPacket::new(108, 1, 0, 0, self.timestamp());
        self.send_packet(&status)?;

        Ok(())
    }

    /// Exchange one byte with the Game Boy via BGB.
    /// Sends a master transfer (cmd=104, sync1), handles intervening packets,
    /// and returns the slave response byte (cmd=105, sync2).
    pub fn exchange_byte(&mut self, send: u8) -> Result<u8, String> {
        let ts = self.timestamp();
        let pkt = BgbPacket::new(104, send, 0x80, 0, ts);
        self.send_packet(&pkt)?;

        loop {
            let resp = self.read_packet()?;
            match resp.command {
                104 => {
                    // Sync1 from BGB — respond with sync2 (acknowledge)
                    let ack = BgbPacket::new(105, 0, 0x80, 0, resp.timestamp);
                    self.send_packet(&ack)?;
                }
                105 => {
                    // Sync2 — slave response with the Game Boy's byte
                    return Ok(resp.data);
                }
                106 => {
                    // Sync3 — acknowledgement, echo it back
                    let ack = BgbPacket::new(106, resp.data, resp.extra1, resp.extra2, resp.timestamp);
                    self.send_packet(&ack)?;
                }
                108 => {
                    // Status query — respond with running status
                    let status = BgbPacket::new(108, 1, 0, 0, resp.timestamp);
                    self.send_packet(&status)?;
                }
                109 => {
                    // Disconnect request
                    return Err("BGB disconnected".into());
                }
                _ => {
                    // Ignore unknown packets
                }
            }
        }
    }

    fn send_packet(&mut self, pkt: &BgbPacket) -> Result<(), String> {
        let bytes = pkt.to_bytes();
        self.stream.write_all(&bytes).map_err(|e| format!("BGB send: {}", e))
    }

    fn read_packet(&mut self) -> Result<BgbPacket, String> {
        let mut buf = [0u8; 8];
        self.stream.read_exact(&mut buf).map_err(|e| format!("BGB read: {}", e))?;
        Ok(BgbPacket::from_bytes(buf))
    }
}
