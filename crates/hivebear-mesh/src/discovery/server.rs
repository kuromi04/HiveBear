use async_trait::async_trait;
use tracing::{debug, warn};

use super::PeerDiscovery;
use crate::error::{MeshError, Result};
use crate::peer::PeerInfo;

/// Client for the centralized coordination server.
///
/// The coordination server is a lightweight HTTP service that maintains
/// a registry of mesh peers. It provides:
/// - POST /register — register a node
/// - POST /heartbeat — maintain registration
/// - GET /peers?model={id}&min_memory={bytes} — find peers
/// - DELETE /deregister — remove registration
pub struct CoordinationServerClient {
    base_url: String,
    http: reqwest::Client,
    node_info: tokio::sync::Mutex<Option<PeerInfo>>,
    /// Last failure, surfaced to the UI so a zero-peer state is explained.
    last_error: std::sync::Mutex<Option<String>>,
}

impl CoordinationServerClient {
    pub fn new(base_url: String) -> Self {
        // Heal the value before it becomes a request prefix: `34.66.188.245`
        // (no scheme) is a relative URL, and a trailing slash turns
        // `{base}/register` into `//register` -> 404.
        let base_url = hivebear_core::config::normalize_coordination_server(&base_url);
        if !base_url.starts_with("https://") {
            tracing::warn!(
                "Coordination server URL uses HTTP ({}). Credentials will be sent in cleartext. \
                 Use https:// in production.",
                base_url
            );
        }
        Self {
            base_url,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            node_info: tokio::sync::Mutex::new(None),
            last_error: std::sync::Mutex::new(None),
        }
    }

    /// Effective, normalized coordination-server URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn set_error(&self, message: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    /// Build an actionable message for a transport-level failure.
    fn unreachable(&self, action: &str, e: &reqwest::Error) -> String {
        format!(
            "coordination server {} unreachable at {} ({e}). Start hivebear-coordinator \
             (`cargo run -p hivebear-coordinator`) or set the correct URL in Settings → Mesh.",
            action, self.base_url
        )
    }
}

#[async_trait]
impl PeerDiscovery for CoordinationServerClient {
    async fn register(&self, info: &PeerInfo) -> Result<()> {
        let url = format!("{}/register", self.base_url);
        debug!("Registering with coordination server at {url}");

        // Store info for heartbeats
        *self.node_info.lock().await = Some(info.clone());

        match self.http.post(&url).json(info).send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!("Registered successfully with coordination server");
                self.set_error(None);
                Ok(())
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!("Registration failed: {status} — {body}");
                let message = format!(
                    "coordination server rejected registration at {url}: HTTP {status}{}{}",
                    if body.trim().is_empty() { "" } else { " — " },
                    body.trim().chars().take(200).collect::<String>()
                );
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                // Report honestly: the caller decides whether to keep running
                // (LAN discovery and retries still work) and the UI shows why
                // there are no peers.
                let message = self.unreachable("not reachable for registration", &e);
                warn!("{message}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) => {
                let message = format!("failed to contact coordination server {url}: {e}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
        }
    }

    async fn find_peers(&self, model_id: &str, min_memory_bytes: u64) -> Result<Vec<PeerInfo>> {
        let url = format!(
            "{}/peers?model={}&min_memory={}",
            self.base_url, model_id, min_memory_bytes
        );
        debug!("Finding peers via {url}");

        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let peers: Vec<PeerInfo> = resp
                    .json()
                    .await
                    .map_err(|e| MeshError::Discovery(format!("Failed to parse peer list: {e}")))?;
                debug!("Found {} peers from coordination server", peers.len());
                self.set_error(None);
                Ok(peers)
            }
            Ok(resp) => {
                let status = resp.status();
                warn!("Peer discovery returned {status}");
                let message = format!(
                    "coordination server returned HTTP {status} for {url}. Check that the URL \
                     points at hivebear-coordinator (no trailing slash, correct port)."
                );
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                let message = self.unreachable("not reachable for peer discovery", &e);
                warn!("{message}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) => Err(MeshError::Discovery(format!("Peer discovery failed: {e}"))),
        }
    }

    async fn heartbeat(&self) -> Result<()> {
        let info = self.node_info.lock().await;
        let info = match info.as_ref() {
            Some(i) => i,
            None => return Err(MeshError::Discovery("Not registered".into())),
        };

        let url = format!("{}/heartbeat", self.base_url);
        match self.http.post(&url).json(info).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.set_error(None);
                Ok(())
            }
            Ok(resp) => {
                // A rejected heartbeat means the coordinator does not know us
                // (or does not accept our payload) — say so, so the caller can
                // re-register instead of silently losing the node.
                let status = resp.status();
                warn!("Heartbeat returned {status}");
                let message =
                    format!("coordination server rejected heartbeat at {url}: HTTP {status}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                let message = self.unreachable("not reachable for heartbeat", &e);
                debug!("{message}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
            Err(e) => {
                let message = format!("heartbeat failed: {e}");
                self.set_error(Some(message.clone()));
                Err(MeshError::Discovery(message))
            }
        }
    }

    async fn deregister(&self) -> Result<()> {
        let info = self.node_info.lock().await;
        let Some(peer) = info.as_ref() else {
            return Ok(());
        };

        // The coordinator removes a node from `?node_id=` (see
        // `hivebear-coordinator`'s `deregister_node`). Without the query
        // parameter the DELETE is a no-op, so the node stayed listed until the
        // coordinator's 60s stale-node sweep — the app reported "left the mesh"
        // while other nodes could still see it.
        let node_id = peer.node_id.to_hex();
        let url = format!("{}/deregister?node_id={}", self.base_url, node_id);
        match self.http.delete(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                debug!("Deregistered node {node_id} from coordination server");
            }
            Ok(resp) => {
                debug!(
                    "Deregister returned {} (non-fatal, the node times out)",
                    resp.status()
                );
            }
            Err(e) => {
                // Non-fatal — node will eventually time out
                debug!("Deregister failed (non-fatal): {e}");
            }
        }

        drop(info);
        *self.node_info.lock().await = None;
        Ok(())
    }

    fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

// ── Extended coordination client methods (swarm management) ──────────

impl CoordinationServerClient {
    /// POST /matchmake — find or create the best swarm for this peer.
    pub async fn matchmake(
        &self,
        node_id: &str,
        total_vram_bytes: i64,
        total_ram_bytes: i64,
        preferred_model: Option<&str>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/matchmake", self.base_url);
        let mut body = serde_json::json!({
            "node_id": node_id,
            "total_vram_bytes": total_vram_bytes,
            "total_ram_bytes": total_ram_bytes,
        });
        if let Some(model) = preferred_model {
            body["preferred_model"] = serde_json::json!(model);
        }

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Matchmake request failed: {e}")))?;

        if resp.status().is_success() {
            resp.json().await.map_err(|e| {
                MeshError::Discovery(format!("Failed to parse matchmake response: {e}"))
            })
        } else {
            let status = resp.status();
            Err(MeshError::Discovery(format!("Matchmake returned {status}")))
        }
    }

    /// POST /swarms/:id/join — join a swarm.
    pub async fn join_swarm(
        &self,
        swarm_id: &str,
        node_id: &str,
        role: &str,
        layers_from: Option<i64>,
        layers_to: Option<i64>,
    ) -> Result<()> {
        let url = format!("{}/swarms/{}/join", self.base_url, swarm_id);
        let body = serde_json::json!({
            "node_id": node_id,
            "role": role,
            "layers_from": layers_from,
            "layers_to": layers_to,
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Join swarm failed: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            Err(MeshError::Discovery(format!(
                "Join swarm returned {status}"
            )))
        }
    }

    /// POST /swarms/:id/leave — leave a swarm.
    pub async fn leave_swarm(&self, swarm_id: &str, node_id: &str) -> Result<()> {
        let url = format!("{}/swarms/{}/leave", self.base_url, swarm_id);
        let body = serde_json::json!({ "node_id": node_id });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Leave swarm failed: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            Err(MeshError::Discovery(format!(
                "Leave swarm returned {status}"
            )))
        }
    }

    /// GET /models — list all models the network can serve.
    pub async fn get_models(&self) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Get models failed: {e}")))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| MeshError::Discovery(format!("Failed to parse models: {e}")))
        } else {
            Ok(Vec::new())
        }
    }

    /// GET /dashboard — aggregate network stats.
    pub async fn get_dashboard(&self) -> Result<serde_json::Value> {
        let url = format!("{}/dashboard", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Get dashboard failed: {e}")))?;

        resp.json()
            .await
            .map_err(|e| MeshError::Discovery(format!("Failed to parse dashboard: {e}")))
    }

    // ── Signal relay methods (NAT traversal coordination) ────────────

    /// POST /signal — send a signaling message to another peer via the coordinator.
    ///
    /// Used for NAT traversal coordination: exchanging external addresses,
    /// coordinating simultaneous QUIC handshakes for hole-punching, etc.
    pub async fn send_signal(
        &self,
        from_node: &str,
        to_node: &str,
        signal_type: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        let url = format!("{}/signal", self.base_url);
        let body = serde_json::json!({
            "from_node": from_node,
            "to_node": to_node,
            "signal_type": signal_type,
            "payload": payload,
        });

        match self.http.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => Ok(()),
            Ok(resp) => {
                let status = resp.status();
                debug!("Signal send returned {status}");
                Ok(()) // Non-fatal — hole punch may still work
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                debug!("Signal relay unreachable (non-fatal): {e}");
                Ok(())
            }
            Err(e) => Err(MeshError::Discovery(format!("Signal send failed: {e}"))),
        }
    }

    /// GET /signals?node_id=... — retrieve pending signaling messages for this node.
    ///
    /// Returns signals from other peers (connection requests, NAT info exchange).
    /// The coordinator clears retrieved signals automatically.
    pub async fn poll_signals(&self, node_id: &str) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/signals?node_id={}", self.base_url, node_id);

        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let signals: Vec<serde_json::Value> = resp
                    .json()
                    .await
                    .map_err(|e| MeshError::Discovery(format!("Failed to parse signals: {e}")))?;
                Ok(signals)
            }
            Ok(_) => Ok(Vec::new()),
            Err(e) if e.is_connect() || e.is_timeout() => {
                debug!("Signal poll failed (non-fatal): {e}");
                Ok(Vec::new())
            }
            Err(e) => Err(MeshError::Discovery(format!("Signal poll failed: {e}"))),
        }
    }

    /// GET /me — node's own profile and contribution status.
    pub async fn get_me(&self) -> Result<serde_json::Value> {
        let url = format!("{}/me", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Get me failed: {e}")))?;

        if resp.status().is_success() {
            resp.json()
                .await
                .map_err(|e| MeshError::Discovery(format!("Failed to parse me: {e}")))
        } else {
            Err(MeshError::Discovery("Not authenticated".into()))
        }
    }

    /// GET /karma/balance?node_id=... — fetch current karma balance and tier.
    pub async fn get_karma_balance(&self, node_id: &str) -> Result<i64> {
        let url = format!("{}/karma/balance?node_id={}", self.base_url, node_id);
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| MeshError::Discovery(e.to_string()))?;
                Ok(json.get("karma").and_then(|k| k.as_i64()).unwrap_or(0))
            }
            _ => Ok(0),
        }
    }

    /// POST /karma/claim — submit a dual-verified work receipt to earn karma.
    pub async fn claim_karma(
        &self,
        client_node_id: &str,
        worker_node_id: &str,
        tokens_processed: u64,
        nonce: &str,
        signature_hash: &str,
    ) -> Result<i64> {
        let url = format!("{}/karma/claim", self.base_url);
        let body = serde_json::json!({
            "receipt": {
                "client_node_id": client_node_id,
                "worker_node_id": worker_node_id,
                "tokens_processed": tokens_processed,
                "timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                "nonce": nonce,
                "signature_hash": signature_hash,
            }
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| MeshError::Discovery(format!("Claim karma network error: {e}")))?;

        if resp.status().is_success() {
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| MeshError::Discovery(e.to_string()))?;
            Ok(json
                .get("new_balance")
                .and_then(|b| b.as_i64())
                .unwrap_or(0))
        } else {
            let err_msg = resp.text().await.unwrap_or_default();
            Err(MeshError::Discovery(format!(
                "Claim karma rejected: {err_msg}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn base_url_is_normalized() {
        // No scheme: would otherwise be a relative URL and every request would
        // fail before leaving the process.
        let client = CoordinationServerClient::new("34.66.188.245".into());
        assert_eq!(client.base_url(), "http://34.66.188.245");

        // Trailing slash: would make the client POST to `//register` (404).
        let client = CoordinationServerClient::new("http://34.66.188.245/".into());
        assert_eq!(client.base_url(), "http://34.66.188.245");
    }

    #[tokio::test]
    async fn explicit_ports_are_preserved() {
        let client = CoordinationServerClient::new("http://example.test:7879".into());
        assert_eq!(client.base_url(), "http://example.test:7879");

        let client = CoordinationServerClient::new("https://mesh.example.test".into());
        assert_eq!(client.base_url(), "https://mesh.example.test");
    }

    #[tokio::test]
    async fn last_error_is_none_before_any_request() {
        let client = CoordinationServerClient::new("http://127.0.0.1:1".into());
        assert_eq!(client.last_error(), None);
    }
}
