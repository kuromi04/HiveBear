use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::config::MeshTier;
use crate::discovery::PeerDiscovery;
use crate::error::Result;
use crate::identity::NodeIdentity;
use crate::nat;
use crate::peer::{NodeId, PeerInfo, PeerState};
use crate::transport::protocol::MeshMessage;
use crate::transport::MeshTransport;
use crate::trust::ReputationManager;

/// Core lifecycle manager for a mesh node.
///
/// Ties together transport, discovery, peer state, and reputation.
pub struct MeshNode {
    pub local_id: NodeId,
    pub signing_key: ed25519_dalek::SigningKey,
    pub transport: Arc<dyn MeshTransport>,
    pub discovery: Arc<dyn PeerDiscovery>,
    pub peers: DashMap<Vec<u8>, (PeerInfo, PeerState)>,
    pub reputation: tokio::sync::Mutex<ReputationManager>,
    pub tier: MeshTier,
    /// Our STUN-discovered external address (populated on start).
    pub external_addr: tokio::sync::RwLock<Option<SocketAddr>>,
    /// STUN servers for NAT detection.
    pub stun_servers: Vec<String>,
    /// Relay servers for symmetric NAT fallback.
    pub relay_servers: Vec<String>,
    /// True once the coordination server accepted this node's registration.
    registered: std::sync::atomic::AtomicBool,
    /// Our own advertised record, kept so registration can be retried.
    local_info: std::sync::Mutex<Option<PeerInfo>>,
    /// Failure that prevented the node from starting (transport/listen).
    start_error: std::sync::Mutex<Option<String>>,
    running: std::sync::atomic::AtomicBool,
    shutdown: Arc<Notify>,
}

impl MeshNode {
    pub fn new(
        transport: Arc<dyn MeshTransport>,
        discovery: Arc<dyn PeerDiscovery>,
        tier: MeshTier,
        reputation_path: Option<std::path::PathBuf>,
    ) -> Self {
        let (local_id, signing_key) = NodeId::generate();
        Self {
            local_id,
            signing_key,
            transport,
            discovery,
            peers: DashMap::new(),
            reputation: tokio::sync::Mutex::new(ReputationManager::new(reputation_path)),
            tier,
            external_addr: tokio::sync::RwLock::new(None),
            stun_servers: vec!["stun.l.google.com:19302".into()],
            relay_servers: vec!["relay.hivebear.com:3478".into()],
            registered: std::sync::atomic::AtomicBool::new(false),
            local_info: std::sync::Mutex::new(None),
            start_error: std::sync::Mutex::new(None),
            running: std::sync::atomic::AtomicBool::new(false),
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Create a node with a pre-existing persistent identity.
    pub fn with_identity(
        identity: NodeIdentity,
        transport: Arc<dyn MeshTransport>,
        discovery: Arc<dyn PeerDiscovery>,
        tier: MeshTier,
        reputation_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            local_id: identity.node_id,
            signing_key: identity.signing_key,
            transport,
            discovery,
            peers: DashMap::new(),
            reputation: tokio::sync::Mutex::new(ReputationManager::new(reputation_path)),
            tier,
            external_addr: tokio::sync::RwLock::new(None),
            stun_servers: vec!["stun.l.google.com:19302".into()],
            relay_servers: vec!["relay.hivebear.com:3478".into()],
            registered: std::sync::atomic::AtomicBool::new(false),
            local_info: std::sync::Mutex::new(None),
            start_error: std::sync::Mutex::new(None),
            running: std::sync::atomic::AtomicBool::new(false),
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// Start the mesh node: listen for connections and register with discovery.
    ///
    /// Automatically discovers external address via STUN before registering,
    /// so the coordinator knows how other peers can reach us.
    pub async fn start(&self, listen_addr: SocketAddr, mut local_info: PeerInfo) -> Result<()> {
        info!("Starting mesh node {} on {}", self.local_id, listen_addr);

        // Discover external address via STUN (non-blocking, best-effort)
        if let Some(stun_server) = self.stun_servers.first() {
            match nat::stun::discover_external_addr(stun_server).await {
                Ok(ext_addr) => {
                    info!("STUN discovered external address: {ext_addr}");
                    *self.external_addr.write().await = Some(ext_addr);
                    local_info.external_addr = Some(ext_addr);
                }
                Err(e) => {
                    debug!("STUN discovery failed (non-fatal): {e}");
                }
            }
        }

        self.transport.listen(listen_addr).await?;
        self.running
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Keep our own record so the maintenance loop can retry registration.
        *self.local_info.lock().unwrap_or_else(|e| e.into_inner()) = Some(local_info.clone());

        // A coordinator failure must NOT stop the node: LAN discovery and the
        // retry loop keep working, and the reason is surfaced to the UI
        // (`MeshNode::last_error`) instead of a silent zero-peer state.
        match self.discovery.register(&local_info).await {
            Ok(()) => {
                self.registered
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                info!("Registered with the coordination server");
            }
            Err(e) => {
                self.registered
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                warn!("Coordinator registration failed (will retry): {e}");
            }
        }

        info!("Mesh node {} is running", self.local_id);
        Ok(())
    }

    /// Start the background maintenance loop.
    ///
    /// This spawns a task that periodically:
    /// - Sends heartbeats to the coordination server (re-registering if needed)
    /// - Pings connected peers and disconnects stale ones
    /// - Discovers and connects to new peers
    pub fn start_maintenance(self: &Arc<Self>) {
        let node = Arc::clone(self);
        let shutdown = Arc::clone(&self.shutdown);

        tokio::spawn(async move {
            let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(60));
            let mut health_interval = tokio::time::interval(Duration::from_secs(30));
            let mut discovery_interval = tokio::time::interval(Duration::from_secs(60));

            // Don't fire immediately for health/discovery
            health_interval.tick().await;
            discovery_interval.tick().await;

            // Run a first discovery pass shortly after startup. Waiting for the
            // first interval kept the UI at zero peers for two minutes even
            // when a coordinator peer or a LAN peer was already reachable.
            tokio::select! {
                _ = shutdown.notified() => return,
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            }
            if !node.is_running() {
                return;
            }
            node.discover_and_connect_peers().await;

            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        debug!("Maintenance loop shutting down");
                        break;
                    }
                    _ = heartbeat_interval.tick() => {
                        if !node.is_running() { break; }
                        node.maintain_registration().await;
                    }
                    _ = health_interval.tick() => {
                        if !node.is_running() { break; }
                        node.check_peer_health().await;
                    }
                    _ = discovery_interval.tick() => {
                        if !node.is_running() { break; }
                        node.discover_and_connect_peers().await;
                    }
                }
            }
        });
    }

    /// Send a heartbeat, re-registering when the coordinator does not know us.
    ///
    /// Registration used to be attempted only once, at startup, and its failure
    /// was swallowed: a node that was offline — or that the coordinator rejected
    /// because of a malformed URL — stayed invisible forever. The coordinator
    /// also auto-registers unknown nodes on `/heartbeat`, so retrying here is
    /// the recovery path for both cases.
    async fn maintain_registration(&self) {
        use std::sync::atomic::Ordering;

        if self.registered.load(Ordering::Relaxed) {
            match self.discovery.heartbeat().await {
                Ok(()) => return,
                Err(e) => {
                    debug!("Heartbeat failed, will re-register: {e}");
                    self.registered.store(false, Ordering::Relaxed);
                }
            }
        }

        let info = {
            let guard = self.local_info.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };
        let Some(info) = info else {
            return;
        };

        match self.discovery.register(&info).await {
            Ok(()) => {
                self.registered.store(true, Ordering::Relaxed);
                info!("Registered with the coordination server");
            }
            Err(e) => debug!("Re-registration failed (will retry): {e}"),
        }
    }

    /// Ping all connected peers and disconnect those that are unresponsive.
    async fn check_peer_health(&self) {
        let peer_ids: Vec<NodeId> = self
            .peers
            .iter()
            .filter(|entry| {
                matches!(
                    entry.value().1,
                    PeerState::Connected | PeerState::Active { .. }
                )
            })
            .map(|entry| entry.value().0.node_id.clone())
            .collect();

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        for peer_id in peer_ids {
            if let Err(e) = self
                .transport
                .send(&peer_id, MeshMessage::Ping { timestamp_ms })
                .await
            {
                debug!("Peer {} unreachable during health check: {e}", peer_id);
                // Mark as disconnected
                let key = peer_id.0.to_bytes().to_vec();
                if let Some(mut entry) = self.peers.get_mut(&key) {
                    entry.value_mut().1 = PeerState::Disconnected;
                }
                let _ = self.transport.disconnect(&peer_id).await;
            }
        }
    }

    /// Discover new peers from the coordination server and connect to them.
    ///
    /// Uses a tiered connection strategy for NAT traversal:
    /// 1. Direct connect to the peer's local address
    /// 2. Try the peer's STUN-discovered external address
    /// 3. Attempt hole-punch (simultaneous QUIC handshake)
    /// 4. Fall back to relay server
    async fn discover_and_connect_peers(&self) {
        let peers = match self.discovery.find_peers("", 0).await {
            Ok(p) => p,
            Err(e) => {
                debug!("Peer discovery failed (non-fatal): {e}");
                return;
            }
        };

        for peer_info in peers {
            // Skip self
            if peer_info.node_id == self.local_id {
                continue;
            }

            // Skip already-connected peers
            let key = peer_info.node_id.0.to_bytes().to_vec();
            if self.peers.contains_key(&key) {
                continue;
            }

            // Check reputation
            {
                let rep = self.reputation.lock().await;
                if rep.is_banned(&peer_info.node_id) {
                    debug!("Skipping banned peer {}", peer_info.node_id);
                    continue;
                }
            }

            match self.connect_with_nat_traversal(&peer_info).await {
                Ok(connected_id) => {
                    info!(
                        "Auto-connected to peer {} at {}",
                        connected_id, peer_info.addr
                    );
                    let key = connected_id.0.to_bytes().to_vec();
                    self.peers.insert(key, (peer_info, PeerState::Connected));
                }
                Err(e) => {
                    debug!(
                        "All connection strategies failed for peer {}: {e}",
                        peer_info.node_id
                    );
                }
            }
        }

        if self.peer_count() > 0 {
            debug!("Connected to {} mesh peers", self.peer_count());
        }
    }

    /// Attempt to connect to a peer using a tiered NAT traversal strategy.
    ///
    /// Tries each method in order, falling through to the next on failure:
    /// 1. Direct connection to advertised address
    /// 2. Connection to STUN-discovered external address
    /// 3. Hole-punch via simultaneous connect
    /// 4. Relay server fallback
    async fn connect_with_nat_traversal(&self, peer: &PeerInfo) -> Result<NodeId> {
        // Strategy 1: Direct connect (works on LAN or when no NAT)
        let direct_timeout = Duration::from_secs(3);
        if let Ok(Ok(id)) =
            tokio::time::timeout(direct_timeout, self.transport.connect(peer.addr)).await
        {
            debug!("Direct connect succeeded to {}", peer.node_id);
            return Ok(id);
        }

        // Strategy 2: Try STUN-discovered external address
        if let Some(ext_addr) = peer.external_addr {
            debug!("Trying external address {} for {}", ext_addr, peer.node_id);
            if let Ok(Ok(id)) =
                tokio::time::timeout(direct_timeout, self.transport.connect(ext_addr)).await
            {
                debug!("External address connect succeeded to {}", peer.node_id);
                return Ok(id);
            }
        }

        // Strategy 3: Hole-punch — both sides send QUIC handshakes simultaneously
        let our_ext = *self.external_addr.read().await;
        if our_ext.is_some() || peer.external_addr.is_some() {
            let target = peer.external_addr.unwrap_or(peer.addr);
            debug!("Attempting hole-punch to {} at {}", peer.node_id, target);
            match nat::holepunch::attempt_holepunch(
                self.transport.as_ref(),
                peer,
                our_ext,
                Duration::from_secs(5),
            )
            .await
            {
                Ok(()) => {
                    // Hole punch succeeded — peer should be connected via transport
                    if let Ok(id) = self.transport.connect(target).await {
                        debug!("Hole-punch succeeded to {}", peer.node_id);
                        return Ok(id);
                    }
                }
                Err(e) => {
                    debug!("Hole-punch failed for {}: {e}", peer.node_id);
                }
            }
        }

        // Strategy 4: Relay fallback
        if !self.relay_servers.is_empty() {
            let relay_client = nat::relay::RelayClient::new(self.relay_servers.clone());
            if relay_client.is_available().await {
                debug!("Requesting relay for {}", peer.node_id);
                match relay_client.allocate_relay(peer).await {
                    Ok(relay_addr) => {
                        if let Ok(id) = self.transport.connect(relay_addr).await {
                            info!("Connected to {} via relay at {}", peer.node_id, relay_addr);
                            return Ok(id);
                        }
                    }
                    Err(e) => {
                        debug!("Relay allocation failed for {}: {e}", peer.node_id);
                    }
                }
            }
        }

        Err(crate::error::MeshError::Transport(format!(
            "All connection strategies exhausted for peer {}",
            peer.node_id
        )))
    }

    /// Start the mesh node in the background without blocking the caller.
    ///
    /// Spawns a task that listens, registers with discovery, and starts
    /// maintenance. Returns immediately so the CLI startup path is never
    /// blocked by network issues. Errors are logged, not propagated.
    pub fn start_background(self: &Arc<Self>, listen_addr: SocketAddr, local_info: PeerInfo) {
        let node = Arc::clone(self);
        // Mark the node as up immediately: callers (and the UI polling
        // `is_running`) must not read "not started" while the STUN probe and
        // the registration request are still in flight.
        node.running
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(async move {
            match node.start(listen_addr, local_info).await {
                Ok(()) => {
                    node.start_maintenance();
                    info!("Mesh node {} running in background", node.local_id);
                }
                Err(e) => {
                    node.running
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    node.set_start_error(Some(format!("mesh transport failed to start: {e}")));
                    warn!("Background mesh start failed (non-fatal): {e}");
                }
            }
        });
    }

    /// Stop the mesh node gracefully.
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping mesh node {}", self.local_id);
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.shutdown.notify_waiters();

        // Disconnect all peers
        let peer_ids: Vec<NodeId> = self
            .peers
            .iter()
            .map(|entry| entry.value().0.node_id.clone())
            .collect();

        for peer_id in peer_ids {
            if let Err(e) = self.transport.disconnect(&peer_id).await {
                warn!("Failed to disconnect from {peer_id}: {e}");
            }
        }
        self.peers.clear();

        self.discovery.deregister().await?;

        info!("Mesh node {} stopped", self.local_id);
        Ok(())
    }

    /// Check if the node is running.
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// True once the coordination server accepted this node's registration.
    ///
    /// `false` while the node is up but running LAN-only, i.e. no other node
    /// can discover it through the coordination server.
    pub fn is_registered(&self) -> bool {
        self.registered.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Most recent user-actionable failure, if any.
    ///
    /// Surfaced by the UI so "not connected" comes with a reason (unreachable
    /// coordinator, HTTP error, transport bind failure) instead of a silent
    /// zero-peer state.
    pub fn last_error(&self) -> Option<String> {
        if let Some(e) = self
            .start_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Some(e);
        }
        self.discovery.last_error()
    }

    fn set_start_error(&self, message: Option<String>) {
        *self.start_error.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    /// Peers we know about (discovered; connected or still connecting).
    pub fn known_peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Check if the node has any connected peers.
    pub fn is_connected(&self) -> bool {
        self.transport.peer_count() > 0
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.transport.peer_count()
    }

    /// Connect to a peer and add them to our peer state.
    pub async fn connect_to_peer(&self, addr: SocketAddr) -> Result<NodeId> {
        let peer_id = self.transport.connect(addr).await?;
        Ok(peer_id)
    }

    /// Get a summary of all known peers and their states.
    pub fn peer_summary(&self) -> Vec<(NodeId, PeerState, f64)> {
        self.peers
            .iter()
            .map(|entry| {
                let (info, state) = entry.value();
                (info.node_id.clone(), state.clone(), info.reputation_score)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::MockDiscovery;
    use crate::transport::mock::{MockRegistry, MockTransport};

    fn make_node() -> MeshNode {
        let (_id, _) = NodeId::generate();
        let registry = MockRegistry::new();
        let (id2, _) = NodeId::generate();
        let transport = Arc::new(MockTransport::new(id2, registry));
        let discovery = Arc::new(MockDiscovery::new());

        MeshNode::new(transport, discovery, MeshTier::Free, None)
    }

    #[test]
    fn test_node_creation() {
        let node = make_node();
        assert!(!node.is_running());
        assert!(!node.is_connected());
        assert_eq!(node.peer_count(), 0);
    }

    #[test]
    fn test_node_with_identity() {
        let identity = NodeIdentity::generate();
        let expected_id = identity.node_id.to_hex();

        let (id, _) = NodeId::generate();
        let registry = MockRegistry::new();
        let transport = Arc::new(MockTransport::new(id, registry));
        let discovery = Arc::new(MockDiscovery::new());

        let node = MeshNode::with_identity(identity, transport, discovery, MeshTier::Free, None);
        assert_eq!(node.local_id.to_hex(), expected_id);
    }

    #[test]
    fn test_new_node_reports_no_registration_and_no_error() {
        let node = make_node();
        assert!(!node.is_registered());
        assert_eq!(node.last_error(), None);
        assert_eq!(node.known_peer_count(), 0);
    }

    fn make_peer() -> PeerInfo {
        PeerInfo {
            node_id: NodeId::generate().0,
            hardware: hivebear_core::profile(),
            available_memory_bytes: 8 * 1024 * 1024 * 1024,
            available_vram_bytes: 0,
            network_bandwidth_mbps: 100.0,
            latency_ms: None,
            tier: MeshTier::Free,
            reputation_score: 1.0,
            addr: "127.0.0.1:7878".parse().unwrap(),
            external_addr: None,
            nat_type: crate::nat::NatType::Unknown,
            latency_map: std::collections::HashMap::new(),
            serving_model_id: None,
            swarm_id: None,
            draft_capability: None,
        }
    }

    /// Discovery source that always fails, to exercise the degraded path.
    struct FailingDiscovery;

    #[async_trait::async_trait]
    impl PeerDiscovery for FailingDiscovery {
        async fn register(&self, _info: &PeerInfo) -> Result<()> {
            Err(crate::error::MeshError::Discovery(
                "coordinator down".into(),
            ))
        }
        async fn find_peers(&self, _model_id: &str, _min: u64) -> Result<Vec<PeerInfo>> {
            Err(crate::error::MeshError::Discovery(
                "coordinator down".into(),
            ))
        }
        async fn heartbeat(&self) -> Result<()> {
            Err(crate::error::MeshError::Discovery(
                "coordinator down".into(),
            ))
        }
        async fn deregister(&self) -> Result<()> {
            Ok(())
        }
        fn last_error(&self) -> Option<String> {
            Some("coordinator down".into())
        }
    }

    #[tokio::test]
    async fn test_maintenance_registers_with_a_working_source() {
        let node = make_node();
        *node.local_info.lock().unwrap() = Some(make_peer());

        node.maintain_registration().await;

        assert!(node.is_registered());
        assert_eq!(node.last_error(), None);
    }

    #[tokio::test]
    async fn test_failed_registration_is_reported_and_not_fatal() {
        let (id, _) = NodeId::generate();
        let registry = MockRegistry::new();
        let transport = Arc::new(MockTransport::new(id, registry));
        let node = MeshNode::new(transport, Arc::new(FailingDiscovery), MeshTier::Free, None);
        *node.local_info.lock().unwrap() = Some(make_peer());

        // Retries must not panic and must keep reporting the reason.
        node.maintain_registration().await;
        assert!(!node.is_registered());
        assert_eq!(node.last_error().as_deref(), Some("coordinator down"));

        node.maintain_registration().await;
        assert!(!node.is_registered());
    }
}
