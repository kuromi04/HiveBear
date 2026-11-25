# Security Policy & Architecture

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| < 0.2.0 | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in HiveBear, please report it responsibly by opening a private security advisory on GitHub (`https://github.com/kuromi04/HiveBear/security/advisories/new`).

- **Do not** open a public GitHub issue for security vulnerabilities.
- Include steps to reproduce, affected components (`hivebear-coordinator`, `hivebear-mesh`, `hivebear-desktop`, `hivebear-cli`), and potential impact.

---

## Implemented Security Controls (`v0.2.41+`)

1. **QUIC TLS 1.3 Transport & NodeId Certificate Binding (`hivebear-mesh`)**
   - All peer-to-peer inference and tensor streams are encrypted with QUIC TLS 1.3 (`rustls` + `quinn`).
   - Self-signed peer X.509 certificates embed the node's Ed25519 `NodeId` inside the Subject Alternative Name (`hivebear-id-<hex>`) and are verified against the handshake `Hello` / `HelloAck` identity to detect active MITM substitution.
   - Compile-time guard (`compile_error!`) prevents building release binaries with `insecure-dev` enabled.

2. **Ed25519 Cryptographic Signatures for Karma & Coordinator Actions (`hivebear-coordinator`, `hivebear-mesh`)**
   - `node_identity.key` is persisted with `0600` permissions on Unix/Android.
   - Karma work receipts (`POST /karma/claim`) require a valid 64-byte Ed25519 signature over `hivebear-karma-v1:...` verified against the client's 32-byte Ed25519 public key (`client_node_id`), plus anti-self-farming, 10-minute timestamp freshness, anti-replay (`processed_receipts`), and a rolling `500 Karma/hour` pair cap.
   - Coordinator control actions (`/register`, `/heartbeat`, `/deregister`, `/signals`) verify Ed25519 `X-Node-Signature` / `X-Node-Timestamp` headers and protect authenticated nodes from unauthorized overwrite, deregistration, or signal polling.

3. **DoS & Memory-Exhaustion Protections**
   - `bincode` wire deserialization enforces strict size limits (`16 MiB` on QUIC streams, `64 KiB` on LAN UDP discovery) to prevent allocation bombs.
   - `MeshWorkerDaemon` caps incoming `messages_json` payloads at `256 KiB`, clamps remote `max_tokens` to `1..=4096`, and bounds the priority inference queue at `32` concurrent pending requests.
   - `hivebear-coordinator` enforces a `64 KiB` HTTP body limit (`DefaultBodyLimit`), caps active registered nodes (`2,000`), and bounds signal queues (`50` messages per peer).

4. **Application & Mobile Hardening (`hivebear-desktop`)**
   - Cloud provider API keys are stored in the native OS Keychain (`hivebear_core::secrets`) and automatically migrated out of `config.toml`.
   - Model registry path resolution strictly rejects `..` path traversal (`resolve_rejects_path_traversal`).
   - Android enforces HTTPS by default for external domains via `res/xml/network_security_config.xml`, scoping cleartext HTTP strictly to the P2P coordinator IP and loopback.
