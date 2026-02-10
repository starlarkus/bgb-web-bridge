/// BGB link cable protocol packet (8 bytes).
///
/// Commands:
///   1   = version handshake
///   104 = sync1 / master transfer (send a byte to the Game Boy)
///   105 = sync2 / slave response (Game Boy sends a byte back)
///   106 = sync3 / acknowledgement
///   108 = status (running/paused flags)
///   109 = want disconnect
#[derive(Debug, Clone, Copy)]
pub struct BgbPacket {
    pub command: u8,
    pub data: u8,
    pub extra1: u8,
    pub extra2: u8,
    pub timestamp: u32,
}

impl BgbPacket {
    pub fn new(command: u8, data: u8, extra1: u8, extra2: u8, timestamp: u32) -> Self {
        Self { command, data, extra1, extra2, timestamp }
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        let ts = self.timestamp.to_le_bytes();
        [self.command, self.data, self.extra1, self.extra2, ts[0], ts[1], ts[2], ts[3]]
    }

    pub fn from_bytes(b: [u8; 8]) -> Self {
        Self {
            command: b[0],
            data: b[1],
            extra1: b[2],
            extra2: b[3],
            timestamp: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        }
    }
}
