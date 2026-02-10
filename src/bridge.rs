use crate::bgb::BgbClient;

/// Magic prefix used by the firmware for timing config and printer mode detection.
/// 0xCAFE repeated 8 times + 0xDEADBEEF repeated 4 times = 32 bytes.
const MAGIC_PREFIX: [u8; 32] = [
    0xCA, 0xFE, 0xCA, 0xFE, 0xCA, 0xFE, 0xCA, 0xFE,
    0xCA, 0xFE, 0xCA, 0xFE, 0xCA, 0xFE, 0xCA, 0xFE,
    0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF,
    0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF,
];

/// Printer mode magic suffix
const PRINTER_SUFFIX: [u8; 4] = [b'P', b'R', b'N', b'T'];

pub struct Bridge {
    bgb: BgbClient,
}

impl Bridge {
    pub fn new(host: &str, port: u16) -> Result<Self, String> {
        let bgb = BgbClient::connect(host, port)?;
        Ok(Self { bgb })
    }

    /// Handle a binary message from the browser.
    /// Mirrors the firmware's `handle_input_data()`:
    /// - 36-byte printer mode magic → return [0x00] (not supported)
    /// - 36-byte timing config magic → return [0x01] (ack)
    /// - Otherwise: exchange each byte via BGB SPI, return all responses
    pub fn handle_message(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        // Check for printer mode magic (36 bytes: 32-byte prefix + "PRNT")
        if data.len() == 36 && data[..32] == MAGIC_PREFIX && data[32..36] == PRINTER_SUFFIX {
            // Printer mode not supported in emulator bridge
            return Ok(vec![0x00]);
        }

        // Check for timing config magic (36 bytes: 32-byte prefix + 4 config bytes)
        if data.len() == 36 && data[..32] == MAGIC_PREFIX {
            // Timing config — acknowledge but ignore (BGB handles its own timing)
            return Ok(vec![0x01]);
        }

        // Normal data: exchange each byte through BGB
        let mut response = Vec::with_capacity(data.len());
        for &b in data {
            let result = self.bgb.exchange_byte(b)?;
            response.push(result);
        }
        Ok(response)
    }
}
