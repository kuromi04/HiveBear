<p align="center">
  <img src="assets/banner.svg" alt="HiveBear Banner" width="800" />
</p>

<p align="center">
  <strong>The world's largest peer-to-peer AI mesh network & local runtime.</strong><br>
  Every device is a node — from High-End GPUs to Laptops and Android devices with Termux.<br>
  <em>Patched, Enhanced & Maintained by <a href="https://github.com/kuromi04">@kuromi04</a></em>
</p>

<p align="center">
  <a href="https://github.com/kuromi04/TermuxHiveBear"><img src="https://img.shields.io/badge/Termux-Supported-brightgreen?logo=android" alt="Termux Support" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License" /></a>
  <img src="https://img.shields.io/badge/version-0.2.0--patched-orange.svg" alt="Version 0.2.0" />
</p>

---

## 🛠️ Patches & Improvements in this Edition (by @kuromi04)

- 🚀 **hivebear-coordinator (P2P Signaling Server):** Added a dedicated, lightweight Rust HTTP server (`crates/hivebear-coordinator`) with Docker support to coordinate P2P mesh discovery, STUN hole-punching, and swarm matchmaking worldwide.
- 📱 **Termux & Mobile Integration ([TermuxHiveBear](https://github.com/kuromi04/TermuxHiveBear)):** Seamlessly connect Android smartphones running Termux to your desktop AI mesh network.
- 📥 **HF Nested Path Download Fix:** Resolved `OS error 3` path errors when downloading HuggingFace models with subdirectories (`Q4_K_M/model.gguf`).
- ⚡ **Resilient Stream Downloads:** Added automatic reconnect and chunk resume logic when HuggingFace or remote servers drop TCP streams.
- 💬 **Chat Persistence & Deserialization Fix:** Conversations persist across app restarts and tab navigation. Fixed `stream_chat` JSON deserialization for text-only assistant responses.

---

## 🌐 P2P Mesh Architecture (Desktop + Mobile Termux)

```
                               ┌─────────────────────────────────────────┐
                               │     HiveBear Coordination Server        │
                               │        (hivebear-coordinator)           │
                               │  https://your-vps.com:7879 / Docker     │
                               └──────────────────┬──────────────────────┘
                                                  │ Handshake & PEX
                      ┌───────────────────────────┴───────────────────────────┐
                      ▼                                                       ▼
       💻 Windows / Mac / Linux PC                             📱 Android Phone (TermuxHiveBear)
       (Runs Layers 1 to 40 of 70B Model)                       (Runs Layers 41 to 60 or Draft Model)
                      ▲                                                       ▲
                      └───────────── Direct P2P Encrypted QUIC Stream ────────┘
```

---

## 🚀 Running the HiveBear Coordinator Server (Docker / VPS)

To host your own P2P discovery & signaling server so devices anywhere in the world can pair automatically:

### Using Docker Compose:

```bash
git clone https://github.com/kuromi04/HiveBear.git
cd HiveBear
docker compose -f crates/hivebear-coordinator/docker-compose.yml up -d
```

### Or using Cargo directly:

```bash
cargo run -p hivebear-coordinator --release -- --port 7879 --bind 0.0.0.0
```

Verify your server is active by visiting: `http://YOUR-SERVER-IP:7879/health`

---

## 📱 Connecting Android Devices via Termux (TermuxHiveBear)

1. Open Termux on Android and clone the Termux repo:
   ```bash
   git clone https://github.com/kuromi04/TermuxHiveBear.git
   cd TermuxHiveBear
   ```
2. Connect to your Desktop PC or Coordinator Server:
   ```bash
   hivebear contribute --coordinator http://YOUR-SERVER-IP:7879 --port 7878
   ```

---

## 🖥️ Installation & Setup Guide

HiveBear runs seamlessly across various platforms, automatically forming a P2P mesh network.

### 🐧 Linux (Ubuntu / Debian / Arch)
**Requirements:** `curl`, `build-essential`, `libwebkit2gtk-4.0-dev` (or `4.1`)
1. Install system dependencies:
   ```bash
   sudo apt update
   sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
   ```
2. Download the AppImage or build from source:
   ```bash
   git clone https://github.com/kuromi04/HiveBear.git
   cd HiveBear/apps/desktop
   npm install
   npx @tauri-apps/cli build
   ```
3. Run the generated `.AppImage` or `.deb` in `target/release/bundle/`.

### 🪟 Windows (10 / 11)
**Requirements:** Node.js (18+), Rust (1.80+), Visual Studio C++ Build Tools.
1. Open PowerShell / Command Prompt and clone the repo:
   ```cmd
   git clone https://github.com/kuromi04/HiveBear.git
   cd HiveBear/apps/desktop
   npm install
   ```
2. Build the desktop installer:
   ```cmd
   npx @tauri-apps/cli build
   ```
3. Run the generated installer: `target/release/bundle/nsis/HiveBear_0.2.0_x64-setup.exe`

### 🍏 macOS (Apple Silicon M1/M2/M3 & Intel)
**Requirements:** Xcode Command Line Tools.
1. Install dependencies and build:
   ```bash
   xcode-select --install
   git clone https://github.com/kuromi04/HiveBear.git
   cd HiveBear/apps/desktop
   npm install
   npx @tauri-apps/cli build --target universal-apple-darwin
   ```
2. Open the generated `.dmg` or `.app` in `target/release/bundle/macos/`.

### 📱 Android (Termux)
Turn your smartphone into an AI compute node!
1. Install [Termux from F-Droid](https://f-droid.org/en/packages/com.termux/).
2. Open Termux and run:
   ```bash
   pkg update && pkg upgrade
   pkg install git rust nodejs build-essential
   git clone https://github.com/kuromi04/TermuxHiveBear.git
   cd TermuxHiveBear
   cargo build --release
   ```
3. Connect to the mesh:
   ```bash
   ./target/release/hivebear contribute --coordinator http://YOUR-VPS-IP:7879 --port 7878
   ```

## 📄 License

MIT License. Designed by Deepmind / BeckhamLabs, enhanced & maintained by [@kuromi04](https://github.com/kuromi04).
