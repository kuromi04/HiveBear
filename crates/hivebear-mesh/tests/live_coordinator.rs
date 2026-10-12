//! End-to-end check against a **real** coordination server.
//!
//! Ignored unless `HIVEBEAR_LIVE_COORDINATOR` is set, so CI stays hermetic.
//! Run it against the deployment the app ships with:
//!
//! ```text
//! HIVEBEAR_LIVE_COORDINATOR=http://34.66.188.245 \
//!   cargo test -p hivebear-mesh --test live_coordinator -- --nocapture
//! ```
//!
//! It exercises exactly the calls the desktop/mobile app makes on startup:
//! `register` -> `find_peers` -> `heartbeat` -> `deregister`, and asserts the
//! node is actually visible in `/peers` while registered and gone afterwards.

use hivebear_mesh::config::MeshTier;
use hivebear_mesh::discovery::server::CoordinationServerClient;
use hivebear_mesh::discovery::PeerDiscovery;
use hivebear_mesh::{NatType, NodeId, PeerInfo};

fn live_coordinator() -> Option<String> {
    std::env::var("HIVEBEAR_LIVE_COORDINATOR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn test_peer(addr: &str) -> PeerInfo {
    let (node_id, _signing_key) = NodeId::generate();
    PeerInfo {
        node_id,
        hardware: hivebear_core::profile(),
        available_memory_bytes: 4 * 1024 * 1024 * 1024,
        available_vram_bytes: 0,
        network_bandwidth_mbps: 100.0,
        latency_ms: None,
        tier: MeshTier::Free,
        reputation_score: 1.0,
        addr: addr.parse().expect("valid socket address"),
        external_addr: None,
        nat_type: NatType::Unknown,
        latency_map: Default::default(),
        serving_model_id: None,
        swarm_id: None,
        draft_capability: None,
    }
}

#[tokio::test]
async fn register_discover_heartbeat_and_deregister_against_the_live_coordinator() {
    let Some(base_url) = live_coordinator() else {
        eprintln!(
            "skipping: set HIVEBEAR_LIVE_COORDINATOR (e.g. http://34.66.188.245) to run this test"
        );
        return;
    };

    let client = CoordinationServerClient::new(base_url.clone());
    let peer = test_peer("127.0.0.1:17878");

    client
        .register(&peer)
        .await
        .unwrap_or_else(|e| panic!("POST {base_url}/register must succeed: {e}"));
    eprintln!("registered {} with {base_url}", peer.node_id.to_hex());

    let peers = client
        .find_peers("", 0)
        .await
        .unwrap_or_else(|e| panic!("GET {base_url}/peers must succeed: {e}"));
    assert!(
        peers.iter().any(|p| p.node_id == peer.node_id),
        "a registered node must be discoverable through /peers (got {} peers)",
        peers.len()
    );

    client
        .heartbeat()
        .await
        .unwrap_or_else(|e| panic!("POST {base_url}/heartbeat must succeed: {e}"));

    client
        .deregister()
        .await
        .unwrap_or_else(|e| panic!("DELETE {base_url}/deregister must succeed: {e}"));

    let after = client
        .find_peers("", 0)
        .await
        .unwrap_or_else(|e| panic!("GET {base_url}/peers after deregister: {e}"));
    assert!(
        !after.iter().any(|p| p.node_id == peer.node_id),
        "a deregistered node must disappear from /peers — DELETE must carry ?node_id="
    );
    eprintln!("deregistered {} cleanly", peer.node_id.to_hex());
}
