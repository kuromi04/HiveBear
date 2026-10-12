use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use crate::discovery::PeerDiscovery;
use crate::error::Result;
use crate::peer::PeerInfo;

/// Composite discovery that queries multiple sources and deduplicates results.
///
/// Combines coordination server, PEX, and optionally mDNS discovery.
pub struct CompositeDiscovery {
    sources: Vec<Arc<dyn PeerDiscovery>>,
    /// The primary source (coordination server) for registration/heartbeat.
    primary: Arc<dyn PeerDiscovery>,
}

impl CompositeDiscovery {
    pub fn new(primary: Arc<dyn PeerDiscovery>) -> Self {
        Self {
            sources: vec![primary.clone()],
            primary,
        }
    }

    /// Add an additional discovery source (e.g., PEX, mDNS, LAN broadcast).
    pub fn add_source(&mut self, source: Arc<dyn PeerDiscovery>) {
        self.sources.push(source);
    }

    /// Number of discovery sources (including primary).
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
}

#[async_trait]
impl PeerDiscovery for CompositeDiscovery {
    async fn register(&self, info: &PeerInfo) -> Result<()> {
        // Every source gets a chance to register: LAN discovery must record our
        // record even when the coordination server is unreachable. The overall
        // call succeeds when at least one source accepted us.
        let mut last_error = None;
        let mut any_ok = false;
        for source in &self.sources {
            match source.register(info).await {
                Ok(()) => any_ok = true,
                Err(e) => {
                    debug!("Discovery source registration failed: {e}");
                    last_error = Some(e);
                }
            }
        }

        match (any_ok, last_error) {
            (true, _) => Ok(()),
            (false, Some(e)) => Err(e),
            (false, None) => Ok(()),
        }
    }

    async fn find_peers(&self, model_id: &str, min_memory_bytes: u64) -> Result<Vec<PeerInfo>> {
        // Query all sources concurrently: a dead coordination server must not
        // delay LAN results by a full request timeout.
        let results = futures::future::join_all(
            self.sources
                .iter()
                .map(|source| source.find_peers(model_id, min_memory_bytes)),
        )
        .await;

        let mut all_peers = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut errors = Vec::new();
        let mut any_ok = false;

        for result in results {
            match result {
                Ok(peers) => {
                    any_ok = true;
                    for peer in peers {
                        let key = peer.node_id.to_hex();
                        if seen_ids.insert(key) {
                            all_peers.push(peer);
                        }
                    }
                }
                Err(e) => {
                    debug!("Discovery source failed (non-fatal): {e}");
                    errors.push(e);
                }
            }
        }

        // Fail only when every source failed; report the first reason.
        if !any_ok && !errors.is_empty() {
            return Err(errors.remove(0));
        }
        Ok(all_peers)
    }

    async fn heartbeat(&self) -> Result<()> {
        // Only heartbeat to primary (coordination server)
        self.primary.heartbeat().await
    }

    async fn deregister(&self) -> Result<()> {
        // Deregister from all sources
        for source in &self.sources {
            let _ = source.deregister().await;
        }
        Ok(())
    }

    fn last_error(&self) -> Option<String> {
        // Getter: must stay side-effect free (the UI polls it every few seconds).
        let errors: Vec<String> = self
            .sources
            .iter()
            .filter_map(|source| source.last_error())
            .collect();
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeshTier;
    use crate::discovery::MockDiscovery;
    use crate::peer::NodeId;

    fn make_peer(reputation: f64) -> PeerInfo {
        let (node_id, _) = NodeId::generate();
        PeerInfo {
            node_id,
            hardware: hivebear_core::profile(),
            available_memory_bytes: 8 * 1024 * 1024 * 1024,
            available_vram_bytes: 0,
            network_bandwidth_mbps: 100.0,
            latency_ms: None,
            tier: MeshTier::Free,
            reputation_score: reputation,
            addr: "127.0.0.1:7878".parse().unwrap(),
            external_addr: None,
            nat_type: crate::nat::NatType::Unknown,
            latency_map: std::collections::HashMap::new(),
            serving_model_id: None,
            swarm_id: None,
            draft_capability: None,
        }
    }

    #[tokio::test]
    async fn test_composite_deduplicates() {
        let peer = make_peer(0.9);

        let source1 = Arc::new(MockDiscovery::with_peers(vec![peer.clone()]));
        let source2 = Arc::new(MockDiscovery::with_peers(vec![peer.clone()]));

        let mut composite = CompositeDiscovery::new(source1);
        composite.add_source(source2);

        let peers = composite.find_peers("", 0).await.unwrap();
        assert_eq!(peers.len(), 1); // Deduplicated
    }

    #[tokio::test]
    async fn test_composite_merges_sources() {
        let peer1 = make_peer(0.9);
        let peer2 = make_peer(0.8);

        let source1 = Arc::new(MockDiscovery::with_peers(vec![peer1]));
        let source2 = Arc::new(MockDiscovery::with_peers(vec![peer2]));

        let mut composite = CompositeDiscovery::new(source1);
        composite.add_source(source2);

        let peers = composite.find_peers("", 0).await.unwrap();
        assert_eq!(peers.len(), 2); // Both unique peers
    }

    #[tokio::test]
    async fn test_composite_source_count() {
        let source1 = Arc::new(MockDiscovery::new());
        let source2 = Arc::new(MockDiscovery::new());

        let mut composite = CompositeDiscovery::new(source1);
        assert_eq!(composite.source_count(), 1);

        composite.add_source(source2);
        assert_eq!(composite.source_count(), 2);
    }

    #[tokio::test]
    async fn test_composite_tolerates_source_failure() {
        // Even if all sources return empty, composite should succeed
        let source1 = Arc::new(MockDiscovery::new());
        let composite = CompositeDiscovery::new(source1);

        let peers = composite.find_peers("model-x", 0).await.unwrap();
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_composite_register_and_find() {
        let source = Arc::new(MockDiscovery::new());
        let composite = CompositeDiscovery::new(source);

        let peer = make_peer(0.9);
        composite.register(&peer).await.unwrap();

        let found = composite.find_peers("", 0).await.unwrap();
        assert_eq!(found.len(), 1);
    }

    #[tokio::test]
    async fn test_composite_deregister() {
        let source = Arc::new(MockDiscovery::new());
        let composite = CompositeDiscovery::new(source);

        let peer = make_peer(0.9);
        composite.register(&peer).await.unwrap();
        composite.deregister().await.unwrap();

        let found = composite.find_peers("", 0).await.unwrap();
        assert!(found.is_empty());
    }
}
