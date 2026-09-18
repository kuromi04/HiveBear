<p align="center">
  <img src="assets/logo-readme.png" alt="HiveBear" width="120" />
</p>

<h1 align="center">HiveBear (Patched Edition)</h1>

<p align="center">
  <strong>The world's largest peer-to-peer AI mesh network & local runtime.</strong><br>
  Every device is a node — from High-End GPUs to Laptops and Android devices with Termux.<br>
  <em>Patched, Enhanced & Maintained by <a href="https://github.com/kuromi04">@kuromi04</a></em>
</p>

<p align="center">
  <a href="https://github.com/kuromi04/TermuxHiveBear"><img src="https://img.shields.io/badge/Termux-Supported-brightgreen?logo=android" alt="Termux Support" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License" /></a>
  <img src="https://img.shields.io/badge/version-0.1.5--patched-orange.svg" alt="Version 0.1.5" />
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

## 🖥️ Building Desktop App from Source (Windows / Mac / Linux)

### Prerequisites:
- Node.js 18+ and Rust (1.80+)
- LLVM / MSVC (on Windows)

### Build Steps:

```bash
# Clone the repository
git clone https://github.com/kuromi04/HiveBear.git
cd HiveBear/apps/desktop

# Install node dependencies
cmd.exe /c "npm install"

# Build desktop installer (NSIS / MSI)
cmd.exe /c "npx @tauri-apps/cli build"
```

The output installer will be saved at:
`target/release/bundle/nsis/HiveBear_0.1.5_x64-setup.exe`

---

## 📄 License

MIT License. Designed by Deepmind / BeckhamLabs, enhanced & maintained by [@kuromi04](https://github.com/kuromi04).
