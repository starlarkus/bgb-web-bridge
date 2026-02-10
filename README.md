# GB Bridge

A bridge program that lets you play [GB Tetris Online](https://tetris.gblink.io) using the [BGB](https://bgb.bircd.org/) Game Boy emulator instead of real hardware. Works on Windows and Linux.

```
Browser (gb-tetris-web)
  | WebSocket (ws://localhost:8767)
  v
GB Bridge (GUI app)
  | TCP (localhost:8765)
  v
BGB Emulator (running Tetris ROM)
```

## Download

Grab the latest binary from the [Releases](../../releases) page:

- **Windows**: `bgb-web-bridge.exe`
- **Linux**: `bgb-web-bridge` (BGB runs via Wine — see below)

## Usage (Windows)

1. **Start BGB** with a Tetris ROM loaded
   - Right-click the BGB window -> **Link** -> **Listen**
   - Default listen port is `8765`

2. **Run GB Bridge**
   - Set the BGB port (default `8765`) and WebSocket port (default `8767`)
   - Click **Start**
   - Status should show "BGB: Connected" once the browser connects

3. **Open the web client**
   - Go to the GB Tetris site
   - Select **BGB Emulator** mode on the connect screen
   - Click **Connect** and enter the bridge port (`8767`)
   - Walk through: Tetris handshake, music select, create/join a game, play!

## Usage (Linux via Wine)

BGB is a Windows-only program, but runs well under Wine. Its TCP link cable feature works over localhost so the native Linux bridge can connect to it.

### One-time setup

1. Install Wine:
   ```bash
   # Debian/Ubuntu
   sudo apt install wine

   # Fedora
   sudo dnf install wine

   # Arch
   sudo pacman -S wine
   ```

2. Download [BGB](https://bgb.bircd.org/) and extract it somewhere (e.g. `~/bgb/`)

### Running

1. **Start BGB under Wine**:
   ```bash
   wine ~/bgb/bgb.exe tetris.gb
   ```
   - Right-click the BGB window -> **Link** -> **Listen**
   - Default listen port is `8765`

2. **Run GB Bridge** (native Linux binary):
   ```bash
   chmod +x bgb-web-bridge
   ./bgb-web-bridge
   ```
   - Configure ports and click **Start**

3. **Open the web client** and follow the same steps as Windows (select BGB Emulator mode, connect)

## Building from Source

Requires [Rust](https://rustup.rs/).

```bash
cargo build --release
```

The binary will be at `target/release/bgb-web-bridge` (Linux) or `target/release/bgb-web-bridge.exe` (Windows).

### Cross-compiling for Windows from Linux

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --target x86_64-pc-windows-gnu --release
```

## How It Works

The bridge translates between the browser's WebSocket messages and BGB's TCP link cable protocol:

- **WebSocket side**: Accepts binary messages from the browser. Each message contains one or more bytes that would normally be sent over USB to the RP2040 adapter.
- **BGB side**: For each byte, performs a Game Boy SPI exchange using BGB's link cable protocol (master transfer command `108`, reads slave response `109`, handles sync keepalive packets `104`).
- **Magic sequences**: The firmware uses special byte patterns to configure timing and enter printer mode. The bridge detects these and returns appropriate acknowledgements without forwarding to BGB.
