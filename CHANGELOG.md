# Changelog

All notable changes to HiveBear are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.38] - 2026-11-22

### Fixed
- **Multi-Turn Context Window Sliding Truncation (`hivebear-inference`)**:
  - Replaced hard `Prompt length exceeds context length` failures in `LlamaCppEngine` (`llamacpp.rs`) and `CandleEngine` (`candle_backend.rs`) with automatic sliding-window conversation truncation that preserves the system prompt and newest turns while reserving generation token headroom (`128..=768` tokens).
- **Singleton `LlamaBackend` Initialization (`llamacpp.rs`)**:
  - Backed `LlamaBackend::init()` with a process-wide `std::sync::OnceLock<LlamaBackend>` singleton (`shared_llama_backend()`) so loading a second model or reloading an existing GGUF model never fails with `BackendAlreadyInitialized`.
- **Structured Tauri Error Toast Parsing (`invoke.ts`, `useInference.ts`)**:
  - Updated `normalize()` in `apps/desktop/src/lib/invoke.ts` to unwrap JSON-encoded `CommandError` payloads (`{"message":"...","code":"..."}`) into clean human-readable messages instead of displaying raw JSON strings in error toasts.
- **Dynamic Header Version Badge (`Sidebar.tsx`)**:
  - Replaced hardcoded `v0.2.0 • P2P Distributed` sidebar header text with `v{__APP_VERSION__} • P2P Distributed`.

## [0.2.37] - 2026-11-22

### Added
- **3-Phase Cryptographic Karma Economy (`hivebear-mesh::karma`, `hivebear-coordinator`)**:
  - Ed25519 signed `WorkReceipt` emitted automatically by Desktop, Android APK, and Termux CLI after remote P2P inference (`10 tokens = 1 Karma`, `CONTRIBUTOR >= 1`, `VIP_ALPHA >= 2,000`).
  - Persistent atomic Karma ledger (`/data/karma_ledger.json`) in `hivebear-coordinator` with Docker volume support, rolling `500 Karma/hr` pair cap, anti-self-farming, 10-minute timestamp freshness, and weekly 10% soft decay above 100 pts.
  - Real `BinaryHeap` priority inference queue in `MeshWorkerDaemon` (`VIP_ALPHA > CONTRIBUTOR > COMMUNITY`) and load-aware VRAM+RAM matchmaking.
  - Live Karma balance & tier badge on the Desktop/Mobile **Dashboard** (`get_karma_status`) and **P2P Mesh Leaderboard**.
- **Lightweight Android ARM64 APK (`HiveBear-arm64.apk`, ~40 MB)** alongside Universal APK (`HiveBear.apk`), signed with v1+v2 (`zipalign -f -p 4`) and monotonic `versionCode`.
- **Direct Local Model Selector & `Chatear` Button**:
  - Added one-tap **Chatear** button on every installed model card in the **Models** browser.
  - Added interactive model dropdown in **Chat** (`ChatInput.tsx`) listing both active `PC Mesh` connections and downloaded local GGUF models (`📱 Cargar local`), plus flat `.gguf` & Android `/sdcard/Download` auto-discovery.
- **Prebuilt Coordinator Binary Workflow (`coordinator.yml`)** and `Dockerfile.prebuilt` for 5-second zero-compile VM deployments.
- **Official `v0.2.37` Desktop (`.exe`) and Android (`.apk`) Screenshots** in `README.md` and the official website (`docs/index.html`).

### Fixed
- **QUIC Idle Timeout on Heavy 9B CPU Models**: enabled 5-second QUIC keepalive (`keep_alive_interval(5s)`), 300-second `max_idle_timeout`, 300-second first-token wait in `stream_mesh_peer_chat`, and background model pre-warming on PC when probed by mobile.
- **Local vs. Mesh Session Routing**: prevented active Mesh sessions (`sessions.len() == 1`) from hijacking loaded local models in `stream_chat`.
- **Android Top Status Bar Overlap**: added safe top inset padding (`max(env(safe-area-inset-top, 0px), 28px)`) in `Layout.tsx`.
- **Default Coordination Server Migration**: updated default coordinator IP to `http://34.63.209.220` with automatic transparent migration of legacy `34.66.188.245` configs in `hivebear-core` and `TermuxHiveBear`.

### Security
- **Ed25519 Coordinator Control Auth**: signed `/register`, `/heartbeat`, `/deregister`, and `/signals` requests (`X-Node-Signature` / `X-Node-Timestamp`) and blocked unauthenticated overwrite, deregistration, or signal polling of authenticated nodes.
- **QUIC TLS 1.3 Certificate <-> `NodeId` Binding**: embedded `hivebear-id-<node_id_hex>` in self-signed peer X.509 SANs and verified identity match during `Hello` / `HelloAck` handshakes.
- **Worker & Coordinator DoS Limits**: bounded `MeshWorkerDaemon` prompts to `256 KiB`, clamped remote `max_tokens` to `1..=4096`, capped pending worker queue at `32` requests, and enforced `64 KiB` HTTP body limit + `2,000` active node cap on `hivebear-coordinator`.
- **Scoped Android Cleartext Config**: added `res/xml/network_security_config.xml` enforcing HTTPS by default for external domains while scoping HTTP to the coordinator IP and loopback.

## [0.1.5] - 2026-03-30

### Added
- Android/mobile support via Tauri
- Community benchmark sharing and hardware-matched model recommendations
- Ollama-compatible `serve` mode with overflow-to-mesh
- Universal AI backend for IDE extensions (OpenAI-compatible API server)
- Comprehensive security hardening across all crates

[0.2.38]: https://github.com/kuromi04/HiveBear/releases/tag/v0.2.38
[0.2.37]: https://github.com/kuromi04/HiveBear/releases/tag/v0.2.37
[0.1.5]: https://github.com/kuromi04/HiveBear/releases/tag/v0.1.5
