//! Dependency-free LAN peer discovery over UDP broadcast.
//!
//! This is the fallback that keeps two nodes on the same network able to find
//! each other when no coordination server is reachable. It deliberately uses
//! UDP **broadcast** (not IP multicast), because multicast reception on Android
//! requires a `WifiManager.MulticastLock` that a Tauri app cannot acquire from
//! Rust; broadcast works with the plain `INTERNET` permission already required
//! by the QUIC transport.
//!
//! Protocol: every [`ANNOUNCE_INTERVAL`] a node broadcasts a `Query` and an
//! `Announce` datagram on [`LAN_DISCOVERY_PORT`]. Listeners answer a `Query`
//! with their own `Announce`. Datagrams are `MAGIC` + a one-byte kind + the
//! bincode-encoded [`PeerInfo`] for announcements. Foreign or malformed
//! datagrams are ignored.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bincode::Options;
use dashmap::DashMap;
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::discovery::PeerDiscovery;
use crate::error::Result;
use crate::peer::PeerInfo;

/// UDP port used for LAN peer announcements.
///
/// The mesh QUIC transport listens on `config.port` (7878) and the coordinator
/// defaults to 7879, so 7877 keeps the three apart.
pub const LAN_DISCOVERY_PORT: u16 = 7877;

/// Protocol prefix; anything else on the port is ignored.
const MAGIC: &[u8] = b"HIVEBEAR-LAN/1";

/// Datagram kinds.
const KIND_QUERY: u8 = b'Q';
const KIND_ANNOUNCE: u8 = b'A';

/// Maximum accepted/sent datagram size (a `PeerInfo` is well under this).
const MAX_DATAGRAM: usize = 16 * 1024;

/// How often we announce ourselves and ask who else is out there.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(10);

/// Peers that stop announcing are forgotten after this long.
const PEER_TTL: Duration = Duration::from_secs(45);

/// A LAN discovery datagram.
#[derive(Debug, Clone)]
pub enum LanMessage {
    /// "Who is on this network?" — listeners reply with `Announce`.
    Query,
    /// "Here I am" — the record other nodes should connect to.
    Announce(Box<PeerInfo>),
}

/// Serialize a discovery datagram. Returns `None` if the peer record cannot be
/// encoded (should not happen for in-memory [`PeerInfo`] values).
pub fn encode_message(msg: &LanMessage) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(128);
    buf.extend_from_slice(MAGIC);
    match msg {
        LanMessage::Query => buf.push(KIND_QUERY),
        LanMessage::Announce(peer) => {
            let payload = bincode::DefaultOptions::new()
                .with_limit(MAX_DATAGRAM as u64)
                .serialize(peer.as_ref())
                .ok()?;
            buf.push(KIND_ANNOUNCE);
            buf.extend_from_slice(&payload);
        }
    }
    Some(buf)
}

/// Parse a discovery datagram, ignoring anything that is not ours or is
/// malformed.
pub fn decode_message(data: &[u8]) -> Option<LanMessage> {
    if data.len() <= MAGIC.len() || !data.starts_with(MAGIC) {
        return None;
    }
    let body = &data[MAGIC.len()..];
    match body.first().copied()? {
        KIND_QUERY if body.len() == 1 => Some(LanMessage::Query),
        KIND_ANNOUNCE => bincode::DefaultOptions::new()
            .with_limit(MAX_DATAGRAM as u64)
            .deserialize(&body[1..])
            .ok()
            .map(|peer: PeerInfo| LanMessage::Announce(Box::new(peer))),
        _ => None,
    }
}

/// Broadcast destinations for announcements.
///
/// Always includes the limited broadcast address; when the local IPv4 address
/// is known, the corresponding subnet-directed broadcast is added too (some
/// stacks drop 255.255.255.255).
fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::from((Ipv4Addr::BROADCAST, port))];
    if let Some(IpAddr::V4(ip)) = crate::nat::discover_local_ip() {
        let octets = ip.octets();
        let directed = Ipv4Addr::new(octets[0], octets[1], octets[2], 255);
        if directed != Ipv4Addr::BROADCAST {
            targets.push(SocketAddr::from((directed, port)));
        }
    }
    targets
}

struct Inner {
    port: u16,
    /// Our own record, set by `register` (the responder answers with this).
    local: std::sync::Mutex<Option<PeerInfo>>,
    /// Recently heard peers with the instant we last heard from them.
    known: DashMap<Vec<u8>, (PeerInfo, Instant)>,
    started: AtomicBool,
    shutdown: Arc<Notify>,
    /// Why the listener could not run, for the diagnostics surfaced to the UI.
    last_error: std::sync::Mutex<Option<String>>,
}

impl Inner {
    fn set_error(&self, msg: Option<String>) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = msg;
    }

    fn local_peer(&self) -> Option<PeerInfo> {
        self.local.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Drop peers we have not heard from within [`PEER_TTL`].
    fn prune(&self) {
        let now = Instant::now();
        self.known
            .retain(|_, (_, seen)| now.duration_since(*seen) < PEER_TTL);
    }

    /// Handle one datagram; returns the optional reply to send back.
    fn handle_datagram(&self, data: &[u8]) -> Option<Vec<u8>> {
        match decode_message(data)? {
            LanMessage::Query => {
                let peer = self.local_peer()?;
                encode_message(&LanMessage::Announce(Box::new(peer)))
            }
            LanMessage::Announce(peer) => {
                if Some(&peer.node_id) == self.local_peer().as_ref().map(|p| &p.node_id) {
                    return None; // Our own broadcast echoed back.
                }
                let key = peer.node_id.0.to_bytes().to_vec();
                self.known.insert(key, (*peer, Instant::now()));
                None
            }
        }
    }

    async fn run(self: Arc<Self>) -> Result<()> {
        let bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, self.port));
        let socket = UdpSocket::bind(bind).await.map_err(|e| {
            crate::error::MeshError::Transport(format!("LAN discovery bind {bind} failed: {e}"))
        })?;
        socket
            .set_broadcast(true)
            .map_err(|e| crate::error::MeshError::Transport(e.to_string()))?;
        self.set_error(None);
        info!("LAN discovery listening on udp/{}", self.port);

        let targets = broadcast_targets(self.port);
        let query = encode_message(&LanMessage::Query);
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut ticker = tokio::time::interval(ANNOUNCE_INTERVAL);

        loop {
            tokio::select! {
                _ = self.shutdown.notified() => break,
                // `readable()` + `try_recv_from` instead of `recv_from(&mut buf)`:
                // the latter borrows the buffer inside the (still alive) select
                // future, which conflicts with reading it in this arm.
                readable = socket.readable() => {
                    if let Err(e) = readable {
                        debug!("LAN discovery socket error: {e}");
                        break;
                    }
                    loop {
                        match socket.try_recv_from(&mut buf) {
                            Ok((n, from)) => {
                                if let Some(reply) = self.handle_datagram(&buf[..n]) {
                                    let _ = socket.send_to(&reply, from).await;
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                debug!("LAN discovery receive error: {e}");
                                break;
                            }
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.prune();
                    let Some(peer) = self.local_peer() else { continue };
                    let announce = encode_message(&LanMessage::Announce(Box::new(peer)));
                    for target in &targets {
                        if let Some(q) = query.as_ref() {
                            let _ = socket.send_to(q, *target).await;
                        }
                        if let Some(a) = announce.as_ref() {
                            let _ = socket.send_to(a, *target).await;
                        }
                    }
                }
            }
        }

        self.set_error(None);
        Ok(())
    }
}

/// LAN discovery source for [`crate::discovery::CompositeDiscovery`].
pub struct LanDiscovery {
    inner: Arc<Inner>,
}

impl LanDiscovery {
    /// Create a discovery source listening on `port`.
    pub fn new(port: u16) -> Self {
        Self {
            inner: Arc::new(Inner {
                port,
                local: std::sync::Mutex::new(None),
                known: DashMap::new(),
                started: AtomicBool::new(false),
                shutdown: Arc::new(Notify::new()),
                last_error: std::sync::Mutex::new(None),
            }),
        }
    }

    /// The UDP port used for announcements.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Record our own peer record and make sure the listener is running.
    ///
    /// Idempotent: only the first call binds the socket.
    pub fn set_local(&self, info: PeerInfo) {
        *self.inner.local.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
        self.start();
    }

    /// Start the listener/announcer task. Idempotent.
    pub fn start(&self) {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            if let Err(e) = Arc::clone(&inner).run().await {
                // Allow a later retry and surface the reason.
                inner.started.store(false, Ordering::SeqCst);
                inner.set_error(Some(e.to_string()));
                warn!("LAN discovery unavailable: {e}");
            }
        });
    }

    /// Stop the listener and forget all peers.
    pub fn stop(&self) {
        self.inner.known.clear();
        *self.inner.local.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.inner.shutdown.notify_one();
        self.inner.started.store(false, Ordering::SeqCst);
    }

    /// Peers heard within the TTL, excluding ourselves.
    pub fn live_peer_count(&self) -> usize {
        self.inner.prune();
        let local = self.inner.local_peer().map(|p| p.node_id);
        self.inner
            .known
            .iter()
            .filter(|entry| Some(entry.value().0.node_id.clone()) != local)
            .count()
    }
}

#[async_trait]
impl PeerDiscovery for LanDiscovery {
    async fn register(&self, info: &PeerInfo) -> Result<()> {
        // Recording our record is all "registration" means on the LAN; the
        // listener answers queries from then on.
        self.set_local(info.clone());
        Ok(())
    }

    async fn find_peers(&self, _model_id: &str, min_memory_bytes: u64) -> Result<Vec<PeerInfo>> {
        self.inner.prune();
        let local = self.inner.local_peer().map(|p| p.node_id);
        Ok(self
            .inner
            .known
            .iter()
            .filter(|entry| {
                Some(entry.value().0.node_id.clone()) != local
                    && entry.value().0.available_memory_bytes >= min_memory_bytes
            })
            .map(|entry| entry.value().0.clone())
            .collect())
    }

    async fn heartbeat(&self) -> Result<()> {
        self.inner.prune();
        Ok(())
    }

    async fn deregister(&self) -> Result<()> {
        self.stop();
        Ok(())
    }

    fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeshTier;
    use crate::nat::NatType;
    use crate::peer::NodeId;

    fn make_peer(addr: &str, memory_gb: u64) -> PeerInfo {
        let (node_id, _) = NodeId::generate();
        PeerInfo {
            node_id,
            hardware: hivebear_core::profile(),
            available_memory_bytes: memory_gb * 1024 * 1024 * 1024,
            available_vram_bytes: 0,
            network_bandwidth_mbps: 100.0,
            latency_ms: None,
            tier: MeshTier::Free,
            reputation_score: 1.0,
            addr: addr.parse().unwrap(),
            external_addr: None,
            nat_type: NatType::Unknown,
            latency_map: std::collections::HashMap::new(),
            serving_model_id: None,
            swarm_id: None,
            draft_capability: None,
        }
    }

    fn remote_peer(peer: PeerInfo) -> Vec<u8> {
        // The datagram a remote node would broadcast for this record.
        encode_message(&LanMessage::Announce(Box::new(peer))).unwrap()
    }

    /// Set our own record without starting the network task, so datagram
    /// handling can be exercised in synchronous tests.
    fn set_local_offline(lan: &LanDiscovery, info: PeerInfo) {
        *lan.inner.local.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
    }

    #[test]
    fn query_roundtrip() {
        let encoded = encode_message(&LanMessage::Query).unwrap();
        assert!(matches!(decode_message(&encoded), Some(LanMessage::Query)));
    }

    #[test]
    fn announce_roundtrip_preserves_the_record() {
        let peer = make_peer("192.168.1.20:7878", 8);
        let encoded = encode_message(&LanMessage::Announce(Box::new(peer.clone()))).unwrap();
        match decode_message(&encoded) {
            Some(LanMessage::Announce(decoded)) => {
                assert_eq!(decoded.node_id, peer.node_id);
                assert_eq!(decoded.addr, peer.addr);
                assert_eq!(decoded.available_memory_bytes, peer.available_memory_bytes);
            }
            other => panic!("unexpected decode: {other:?}"),
        }
    }

    #[test]
    fn roundtrip_survives_a_non_empty_latency_map() {
        // `PeerInfo.latency_map` is keyed by `NodeId`, which serializes as raw
        // bytes — JSON cannot represent that, so the LAN wire format must stay
        // binary. This pins that choice.
        let mut peer = make_peer("10.0.0.7:7878", 16);
        let (other_id, _) = NodeId::generate();
        peer.latency_map.insert(other_id, 12.5);
        let encoded = encode_message(&LanMessage::Announce(Box::new(peer.clone()))).unwrap();
        match decode_message(&encoded) {
            Some(LanMessage::Announce(decoded)) => {
                assert_eq!(decoded.latency_map.len(), 1);
            }
            other => panic!("unexpected decode: {other:?}"),
        }
    }

    #[test]
    fn foreign_and_malformed_datagrams_are_ignored() {
        assert!(decode_message(b"").is_none());
        assert!(decode_message(b"GET / HTTP/1.1").is_none());
        assert!(decode_message(b"HIVEBEAR-LAN/1").is_none());
        assert!(decode_message(b"HIVEBEAR-LAN/1X").is_none());
        assert!(decode_message(b"HIVEBEAR-LAN/1A\xff\xff").is_none());
    }

    #[test]
    fn listener_ignores_foreign_datagrams() {
        let lan = LanDiscovery::new(0);
        set_local_offline(&lan, make_peer("192.168.1.10:7878", 8));
        assert!(lan.inner.handle_datagram(b"hello").is_none());
        assert_eq!(lan.live_peer_count(), 0);
    }

    #[test]
    fn query_is_answered_with_our_own_record() {
        let lan = LanDiscovery::new(0);
        let me = make_peer("192.168.1.10:7878", 8);
        let my_id = me.node_id.clone();
        set_local_offline(&lan, me);

        let reply = lan
            .inner
            .handle_datagram(&encode_message(&LanMessage::Query).unwrap())
            .expect("a query must be answered once we have a local record");
        match decode_message(&reply) {
            Some(LanMessage::Announce(peer)) => assert_eq!(peer.node_id, my_id),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn query_is_not_answered_before_registration() {
        let lan = LanDiscovery::new(0);
        assert!(lan
            .inner
            .handle_datagram(&encode_message(&LanMessage::Query).unwrap())
            .is_none());
    }

    #[test]
    fn announcements_are_recorded_and_our_own_is_ignored() {
        let lan = LanDiscovery::new(0);
        let me = make_peer("192.168.1.10:7878", 8);
        let me_echo = me.clone();
        set_local_offline(&lan, me);

        let remote = make_peer("192.168.1.20:7878", 8);
        assert!(lan.inner.handle_datagram(&remote_peer(remote)).is_none());
        assert_eq!(lan.live_peer_count(), 1);

        // Our own announcement echoed back must not count as a peer.
        lan.inner.handle_datagram(&remote_peer(me_echo));
        assert_eq!(lan.live_peer_count(), 1);
    }

    #[tokio::test]
    async fn find_peers_filters_memory_and_self() {
        let lan = LanDiscovery::new(0);
        set_local_offline(&lan, make_peer("192.168.1.10:7878", 8));

        let small = make_peer("192.168.1.30:7878", 4);
        let large = make_peer("192.168.1.40:7878", 64);
        lan.inner.handle_datagram(&remote_peer(small));
        lan.inner.handle_datagram(&remote_peer(large));

        assert_eq!(lan.find_peers("", 0).await.unwrap().len(), 2);
        assert_eq!(
            lan.find_peers("", 16 * 1024 * 1024 * 1024)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn deregister_clears_peers() {
        let lan = LanDiscovery::new(0);
        set_local_offline(&lan, make_peer("192.168.1.10:7878", 8));
        lan.inner
            .handle_datagram(&remote_peer(make_peer("192.168.1.20:7878", 8)));
        assert_eq!(lan.live_peer_count(), 1);

        lan.deregister().await.unwrap();
        assert_eq!(lan.live_peer_count(), 0);
        assert!(lan.find_peers("", 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn two_listeners_on_the_same_loopback_port_exchange_records() {
        // End-to-end proof that the datagram handler produces exactly what the
        // peer's handler consumes, over a real socket pair.
        let a_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b_addr = b_sock.local_addr().unwrap();

        let a = LanDiscovery::new(0);
        let b = LanDiscovery::new(0);
        let a_peer = make_peer("127.0.0.1:7878", 8);
        let b_peer = make_peer("127.0.0.1:7879", 16);
        set_local_offline(&a, a_peer);
        set_local_offline(&b, b_peer);

        // A asks; B answers with its record; A records it.
        let query = encode_message(&LanMessage::Query).unwrap();
        a_sock.send_to(&query, b_addr).await.unwrap();
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let (n, from) = b_sock.recv_from(&mut buf).await.unwrap();
        let reply = b.inner.handle_datagram(&buf[..n]).expect("reply");
        b_sock.send_to(&reply, from).await.unwrap();

        let (n, _) = a_sock.recv_from(&mut buf).await.unwrap();
        assert!(a.inner.handle_datagram(&buf[..n]).is_none());
        assert_eq!(a.live_peer_count(), 1);
        assert_eq!(b.live_peer_count(), 0);
    }
}
