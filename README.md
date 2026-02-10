# GB Bridge

A Windows bridge program that lets you play [GB Tetris Online](https://tetris.gblink.io) using the [BGB](https://bgb.bircd.org/) Game Boy emulator instead of real hardware.

```
Browser (gb-tetris-web)
  | WebSocket (ws://localhost:8767)
  v
GB Bridge (.exe with GUI)
  | TCP (localhost:8765)
  v
BGB Emulator (running Tetris ROM)
```

## Download

Grab the latest `gb-bridge.exe` from the [Releases](../../releases) page.

## Usage

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

## Building from Source

Requires [Rust](https://rustup.rs/).

```bash
cargo build --release
```

The binary will be at `target/release/gb-bridge.exe`.

### Cross-compiling from Linux

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --target x86_64-pc-windows-gnu --release
```

## How It Works

The bridge translates between the browser's WebSocket messages and BGB's TCP link cable protocol:

- **WebSocket side**: Accepts binary messages from the browser. Each message contains one or more bytes that would normally be sent over USB to the RP2040 adapter.
- **BGB side**: For each byte, performs a Game Boy SPI exchange using BGB's link cable protocol (master transfer command `108`, reads slave response `109`, handles sync keepalive packets `104`).
- **Magic sequences**: The firmware uses special byte patterns to configure timing and enter printer mode. The bridge detects these and returns appropriate acknowledgements without forwarding to BGB.
