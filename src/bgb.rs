use std::io::{Read, Write};
use std::net::TcpStream;

use crate::protocol::BgbPacket;

pub struct BgbClient {
    stream: TcpStream,
    timestamp: u32,
}

impl BgbClient {
    /// Connect to BGB and perform the version handshake.
    pub fn connect(host: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).map_err(|e| format!("TCP connect to {}: {}", addr, e))?;
        stream.set_nodelay(true).ok();

        let mut client = Self { stream, timestamp: 0 };
        client.handshake()?;
        Ok(client)
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
        Ok(())
    }

    /// Exchange one byte with the Game Boy via BGB.
    /// Sends a master transfer (cmd=108), handles intervening sync packets,
    /// and returns the slave response byte (cmd=109).
    pub fn exchange_byte(&mut self, send: u8) -> Result<u8, String> {
        self.timestamp = self.timestamp.wrapping_add(1);
        let pkt = BgbPacket::new(108, send, 0x80, 0, self.timestamp);
        self.send_packet(&pkt)?;

        loop {
            let resp = self.read_packet()?;
            match resp.command {
                104 => {
                    // Sync packet — echo it back to keep BGB in lockstep
                    self.send_packet(&resp)?;
                }
                109 => {
                    // Slave response — this is the byte the Game Boy sent back
                    return Ok(resp.data);
                }
                _ => {
                    // Ignore other packets
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
