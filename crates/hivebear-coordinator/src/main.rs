use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::{delete, get, post};
use axum::Router;
use clap::Parser;
use dashmap::DashMap;
use hivebear_mesh::karma::{
    apply_weekly_decay, cloud_karma_cost_for_tokens, karma_earned_for_tokens,
    validate_ed25519_node_id, verify_coordinator_action, KarmaTier, WorkReceipt,
    MAX_PAIR_KARMA_PER_HOUR, MAX_TOKENS_PER_RECEIPT, RECEIPT_MAX_AGE_SECS,
};
use hivebear_mesh::PeerInfo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_WEEK: u64 = 7 * SECONDS_PER_DAY;
const MAX_ACTIVE_NODES: usize = 2_000;
const MAX_HTTP_BODY_BYTES: usize = 64 * 1024;

#[derive(Parser)]
#[command(
    name = "hivebear-coordinator",
    about = "HiveBear P2P Mesh Coordination & Signaling Server",
    version
)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "7879")]
    port: u16,

    /// Address to bind to
    #[arg(short, long, default_value = "0.0.0.0")]
    bind: String,

    /// Directory to persist the Karma ledger across restarts
    #[arg(long, default_value = "./data")]
    data_dir: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedLedger {
    balances: HashMap<String, i64>,
    last_decay_ts: u64,
    #[serde(default)]
    processed_receipts: HashMap<String, u64>,
}

struct NodeEntry {
    info: PeerInfo,
    last_seen: Instant,
    authenticated: bool,
}

struct CoordinatorState {
    nodes: DashMap<String, NodeEntry>,
    signals: DashMap<String, Vec<serde_json::Value>>,
    karma: DashMap<String, i64>,
    /// Receipt replay cache: `receipt_key` -> unix timestamp seconds
    processed_receipts: DashMap<String, u64>,
    /// Rolling hourly rate limit per `(client, worker)` pair: `(window_start_ts, karma_earned)`
    pair_hourly_karma: DashMap<String, (u64, i64)>,
    last_decay_ts: AtomicU64,
    ledger_path: PathBuf,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl CoordinatorState {
    fn load_or_new(data_dir: &std::path::Path) -> Self {
        let ledger_path = data_dir.join("karma_ledger.json");
        let karma = DashMap::new();
        let processed_receipts = DashMap::new();
        let mut last_decay_ts = unix_now();

        if let Ok(bytes) = std::fs::read(&ledger_path) {
            if let Ok(ledger) = serde_json::from_slice::<PersistedLedger>(&bytes) {
                for (k, v) in ledger.balances {
                    karma.insert(k, v);
                }
                let cutoff = unix_now().saturating_sub(SECONDS_PER_DAY);
                for (k, ts) in ledger.processed_receipts {
                    if ts >= cutoff {
                        processed_receipts.insert(k, ts);
                    }
                }
                if ledger.last_decay_ts > 0 {
                    last_decay_ts = ledger.last_decay_ts;
                }
                info!(
                    "Loaded persisted Karma ledger ({} accounts) from {}",
                    karma.len(),
                    ledger_path.display()
                );
            }
        }

        Self {
            nodes: DashMap::new(),
            signals: DashMap::new(),
            karma,
            processed_receipts,
            pair_hourly_karma: DashMap::new(),
            last_decay_ts: AtomicU64::new(last_decay_ts),
            ledger_path,
        }
    }

    fn persist_ledger(&self) {
        if let Some(parent) = self.ledger_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!("Failed to create ledger directory: {e}");
                return;
            }
        }

        let balances: HashMap<String, i64> = self
            .karma
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect();
        let cutoff = unix_now().saturating_sub(SECONDS_PER_DAY);
        let processed_receipts: HashMap<String, u64> = self
            .processed_receipts
            .iter()
            .filter(|e| *e.value() >= cutoff)
            .map(|e| (e.key().clone(), *e.value()))
            .collect();

        let ledger = PersistedLedger {
            balances,
            last_decay_ts: self.last_decay_ts.load(Ordering::Relaxed),
            processed_receipts,
        };

        let tmp_path = self.ledger_path.with_extension("json.tmp");
        match serde_json::to_vec_pretty(&ledger) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&tmp_path, data) {
                    warn!("Failed to write temporary Karma ledger: {e}");
                    return;
                }
                if let Err(e) = std::fs::rename(&tmp_path, &self.ledger_path) {
                    warn!("Failed to atomically persist Karma ledger: {e}");
                }
            }
            Err(e) => warn!("Failed to serialize Karma ledger: {e}"),
        }
    }

    fn maybe_apply_weekly_decay(&self) {
        let now = unix_now();
        let last = self.last_decay_ts.load(Ordering::Relaxed);
        if now.saturating_sub(last) < SECONDS_PER_WEEK {
            return;
        }
        if self
            .last_decay_ts
            .compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            for mut entry in self.karma.iter_mut() {
                let updated = apply_weekly_decay(*entry.value());
                *entry.value_mut() = updated;
            }
            info!("Applied weekly 10% Karma soft decay to inactive balances > 100");
            self.persist_ledger();
        }
    }
}

type SharedState = Arc<CoordinatorState>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("hivebear_coordinator=info,info")),
        )
        .init();

    let args = Args::parse();
    let state = Arc::new(CoordinatorState::load_or_new(&args.data_dir));

    // Background task: prune stale nodes (60s), expire old receipts/rate-limits, and apply weekly decay
    let prune_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            let now = Instant::now();
            prune_state.nodes.retain(|id, entry| {
                let active = now.duration_since(entry.last_seen) < Duration::from_secs(60);
                if !active {
                    info!("Pruning inactive node: {}", id);
                    prune_state.signals.remove(id);
                }
                active
            });

            let now_unix = unix_now();
            let receipt_cutoff = now_unix.saturating_sub(SECONDS_PER_DAY);
            prune_state
                .processed_receipts
                .retain(|_, ts| *ts >= receipt_cutoff);
            let hour_cutoff = now_unix.saturating_sub(SECONDS_PER_HOUR);
            prune_state
                .pair_hourly_karma
                .retain(|_, (window_ts, _)| *window_ts >= hour_cutoff);

            prune_state.maybe_apply_weekly_decay();
        }
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(health_check))
        .route("/health", get(health_check))
        .route("/register", post(register_node))
        .route("/heartbeat", post(heartbeat_node))
        .route("/peers", get(list_peers))
        .route("/deregister", delete(deregister_node))
        .route("/signal", post(send_signal))
        .route("/signals", get(poll_signals))
        .route("/matchmake", post(matchmake))
        .route("/dashboard", get(dashboard))
        .route("/karma/balance", get(get_karma_balance))
        .route("/karma/claim", post(claim_karma))
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(cors)
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    info!(
        "🚀 HiveBear Coordination Server starting on http://{}",
        addr
    );
    info!("Author / Maintained by @kuromi04 & HiveBear Community");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Extract and verify optional Ed25519 signature headers (`X-Node-Signature`, `X-Node-Timestamp`).
/// Returns `Ok(true)` if signed and valid, `Ok(false)` if no signature header was sent,
/// or `Err(StatusCode::FORBIDDEN)` if a signature was present and invalid/expired.
fn check_node_signature(
    headers: &HeaderMap,
    node_id: &str,
    action: &str,
) -> Result<bool, StatusCode> {
    let Some(sig_val) = headers.get("x-node-signature") else {
        return Ok(false);
    };
    let sig_hex = sig_val.to_str().map_err(|_| StatusCode::FORBIDDEN)?;
    let ts: u64 = headers
        .get("x-node-timestamp")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .ok_or(StatusCode::FORBIDDEN)?;

    verify_coordinator_action(node_id, action, ts, sig_hex).map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(true)
}

async fn health_check() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "hivebear-coordinator",
        "version": "0.2.0",
        "patched_by": "@kuromi04",
    }))
}

async fn register_node(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(info): Json<PeerInfo>,
) -> StatusCode {
    let node_id = info.node_id.to_hex();
    if validate_ed25519_node_id(&node_id).is_err() {
        return StatusCode::BAD_REQUEST;
    }
    let signed = match check_node_signature(&headers, &node_id, "register") {
        Ok(s) => s,
        Err(code) => return code,
    };

    // Protect already-authenticated nodes from unsigned overwrite
    if let Some(existing) = state.nodes.get(&node_id) {
        if existing.authenticated && !signed {
            return StatusCode::FORBIDDEN;
        }
    } else if state.nodes.len() >= MAX_ACTIVE_NODES {
        return StatusCode::TOO_MANY_REQUESTS;
    }

    info!(
        "Registering node {} ({}, signed={})",
        node_id, info.addr, signed
    );
    state.nodes.insert(
        node_id,
        NodeEntry {
            info,
            last_seen: Instant::now(),
            authenticated: signed,
        },
    );
    StatusCode::OK
}

async fn heartbeat_node(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(info): Json<PeerInfo>,
) -> StatusCode {
    let node_id = info.node_id.to_hex();
    if validate_ed25519_node_id(&node_id).is_err() {
        return StatusCode::BAD_REQUEST;
    }
    let signed = match check_node_signature(&headers, &node_id, "heartbeat") {
        Ok(s) => s,
        Err(code) => return code,
    };

    if let Some(mut entry) = state.nodes.get_mut(&node_id) {
        if entry.authenticated && !signed {
            return StatusCode::FORBIDDEN;
        }
        entry.info = info;
        entry.last_seen = Instant::now();
        if signed {
            entry.authenticated = true;
        }
        StatusCode::OK
    } else {
        if state.nodes.len() >= MAX_ACTIVE_NODES {
            return StatusCode::TOO_MANY_REQUESTS;
        }
        state.nodes.insert(
            node_id,
            NodeEntry {
                info,
                last_seen: Instant::now(),
                authenticated: signed,
            },
        );
        StatusCode::OK
    }
}

#[derive(Deserialize)]
struct PeersQuery {
    model: Option<String>,
    min_memory: Option<u64>,
}

async fn list_peers(
    State(state): State<SharedState>,
    Query(query): Query<PeersQuery>,
) -> Json<Vec<PeerInfo>> {
    let min_mem = query.min_memory.unwrap_or(0);
    let target_model = query.model.unwrap_or_default();

    let mut result = Vec::new();
    for entry in state.nodes.iter() {
        let info = &entry.value().info;
        if info.available_memory_bytes >= min_mem
            && (target_model.is_empty()
                || info
                    .serving_model_id
                    .as_ref()
                    .map(|m| m.contains(&target_model))
                    .unwrap_or(true))
        {
            result.push(info.clone());
        }
    }
    Json(result)
}

#[derive(Deserialize)]
struct DeregisterQuery {
    node_id: Option<String>,
}

async fn deregister_node(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<DeregisterQuery>,
) -> StatusCode {
    if let Some(id) = query.node_id {
        let signed = match check_node_signature(&headers, &id, "deregister") {
            Ok(s) => s,
            Err(code) => return code,
        };
        if let Some(existing) = state.nodes.get(&id) {
            if existing.authenticated && !signed {
                return StatusCode::FORBIDDEN;
            }
        }
        info!("Deregistering node {} (signed={})", id, signed);
        state.nodes.remove(&id);
        state.signals.remove(&id);
    }
    StatusCode::OK
}

#[derive(Deserialize, Serialize)]
struct SignalMessage {
    from_node: String,
    to_node: String,
    signal_type: String,
    payload: serde_json::Value,
}

async fn send_signal(
    State(state): State<SharedState>,
    Json(signal): Json<SignalMessage>,
) -> StatusCode {
    if validate_ed25519_node_id(&signal.from_node).is_err()
        || validate_ed25519_node_id(&signal.to_node).is_err()
    {
        return StatusCode::BAD_REQUEST;
    }
    let to_node = signal.to_node.clone();
    let Ok(val) = serde_json::to_value(signal) else {
        return StatusCode::BAD_REQUEST;
    };

    // SECURITY FIX: Cap the number of signals and total signal queues to prevent Memory Leak / DoS
    if !state.signals.contains_key(&to_node) && state.signals.len() >= MAX_ACTIVE_NODES {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let mut entry = state.signals.entry(to_node).or_default();
    if entry.len() < 50 {
        entry.push(val);
        StatusCode::OK
    } else {
        StatusCode::TOO_MANY_REQUESTS
    }
}

#[derive(Deserialize)]
struct PollSignalQuery {
    node_id: String,
}

async fn poll_signals(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<PollSignalQuery>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let signed = check_node_signature(&headers, &query.node_id, "signals")?;
    if let Some(existing) = state.nodes.get(&query.node_id) {
        if existing.authenticated && !signed {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    if let Some((_, signals)) = state.signals.remove(&query.node_id) {
        Ok(Json(signals))
    } else {
        Ok(Json(Vec::new()))
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct MatchmakeRequest {
    node_id: String,
    total_vram_bytes: Option<i64>,
    total_ram_bytes: Option<i64>,
    preferred_model: Option<String>,
}

async fn matchmake(
    State(state): State<SharedState>,
    Json(req): Json<MatchmakeRequest>,
) -> Json<serde_json::Value> {
    let available_peers = state.nodes.len();
    let requester_karma = state.karma.get(&req.node_id).map(|k| *k).unwrap_or(0);
    let requester_tier = KarmaTier::from_balance(requester_karma);

    // Load-aware worker ranking: prioritize available VRAM + RAM capacity, with a small
    // reliability tie-breaker from Karma, instead of blindly overloading one high-karma node.
    let mut ranked_peers: Vec<_> = state
        .nodes
        .iter()
        .filter(|entry| entry.key() != &req.node_id)
        .map(|entry| {
            let p = &entry.value().info;
            let id = p.node_id.to_hex();
            let peer_karma = state.karma.get(&id).map(|k| *k).unwrap_or(0);
            let capacity_score: u64 = p
                .available_vram_bytes
                .saturating_mul(2)
                .saturating_add(p.available_memory_bytes);
            (id, p.addr.to_string(), peer_karma, capacity_score)
        })
        .collect();

    ranked_peers.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| b.2.cmp(&a.2)));

    Json(json!({
        "swarm_id": format!("swarm-{}", req.preferred_model.unwrap_or_else(|| "default".to_string())),
        "assigned_role": "worker",
        "total_peers_in_swarm": available_peers,
        "requester_karma": requester_karma,
        "priority_tier": requester_tier.as_str(),
        "ranked_peers": ranked_peers.into_iter().map(|(id, addr, karma, _)| json!({
            "node_id": id,
            "addr": addr,
            "karma": karma
        })).collect::<Vec<_>>()
    }))
}

#[derive(Deserialize)]
struct KarmaBalanceQuery {
    node_id: String,
}

async fn get_karma_balance(
    State(state): State<SharedState>,
    Query(query): Query<KarmaBalanceQuery>,
) -> Json<serde_json::Value> {
    let balance = state.karma.get(&query.node_id).map(|k| *k).unwrap_or(0);
    let tier = KarmaTier::from_balance(balance);
    Json(json!({
        "node_id": query.node_id,
        "karma": balance,
        "tier": tier.as_str(),
        "priority_rank": tier.priority_rank(),
        "next_tier_at": KarmaTier::next_tier_threshold(balance),
    }))
}

#[derive(Deserialize)]
struct ClaimKarmaRequest {
    receipt: WorkReceipt,
}

async fn claim_karma(
    State(state): State<SharedState>,
    Json(req): Json<ClaimKarmaRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let r = &req.receipt;

    // 1. Block self-farming (a node cannot award Karma to itself)
    if r.client_node_id == r.worker_node_id {
        return Err((
            StatusCode::FORBIDDEN,
            "Self-farming rejected: client_node_id and worker_node_id must differ".to_string(),
        ));
    }

    // 2. Cryptographic Ed25519 signature verification against client's public key
    if let Err(reason) = r.verify_signature() {
        return Err((StatusCode::FORBIDDEN, reason));
    }

    // 3. Timestamp freshness check (prevents old receipt replay after cache pruning)
    let now = unix_now();
    if now.abs_diff(r.timestamp) > RECEIPT_MAX_AGE_SECS {
        return Err((
            StatusCode::BAD_REQUEST,
            "Work receipt timestamp expired or too far in the future".to_string(),
        ));
    }

    // 4. Token bounds check
    if r.tokens_processed == 0 || r.tokens_processed > MAX_TOKENS_PER_RECEIPT {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid token range in work receipt".to_string(),
        ));
    }

    // 5. Anti-Replay verification
    let receipt_key = format!("{}:{}:{}", r.client_node_id, r.nonce, r.timestamp);
    if state.processed_receipts.contains_key(&receipt_key) {
        return Err((
            StatusCode::CONFLICT,
            "Receipt already claimed (anti-replay check failed)".to_string(),
        ));
    }

    // 6. Calculate Karma (10 tokens = 1 Karma, minimum 1) & rolling hourly pair cap
    let points = karma_earned_for_tokens(r.tokens_processed);
    let pair_key = format!("{}:{}", r.client_node_id, r.worker_node_id);
    {
        let mut pair_entry = state.pair_hourly_karma.entry(pair_key).or_insert((now, 0));
        if now.saturating_sub(pair_entry.0) >= SECONDS_PER_HOUR {
            *pair_entry = (now, 0);
        }
        if pair_entry.1 + points > MAX_PAIR_KARMA_PER_HOUR {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "Hourly Karma cap ({MAX_PAIR_KARMA_PER_HOUR} pts/hr) reached for this client-worker pair"
                ),
            ));
        }
        pair_entry.1 += points;
    }

    // 7. Credit worker Karma
    let new_balance = {
        let mut current = state.karma.entry(r.worker_node_id.clone()).or_insert(0);
        *current += points;
        *current
    };

    // 8. Gentle cloud sink (LAN is 100% free; community users at 0 are never blocked)
    let cloud_cost = cloud_karma_cost_for_tokens(r.tokens_processed, r.is_lan);
    let client_balance = if cloud_cost > 0 {
        let mut client_entry = state.karma.entry(r.client_node_id.clone()).or_insert(0);
        *client_entry = (*client_entry - cloud_cost).max(0);
        *client_entry
    } else {
        state.karma.get(&r.client_node_id).map(|v| *v).unwrap_or(0)
    };

    // 9. Record receipt as processed & persist ledger atomically to disk
    state.processed_receipts.insert(receipt_key, now);
    state.persist_ledger();

    let worker_tier = KarmaTier::from_balance(new_balance);
    info!(
        "🐺 Karma awarded! Worker {} received +{} Karma ({} tokens, lan={}) from {} -> New Balance: {} ({})",
        r.worker_node_id,
        points,
        r.tokens_processed,
        r.is_lan,
        r.client_node_id,
        new_balance,
        worker_tier.as_str()
    );

    Ok(Json(json!({
        "status": "success",
        "worker_node_id": r.worker_node_id,
        "karma_awarded": points,
        "new_balance": new_balance,
        "worker_tier": worker_tier.as_str(),
        "client_karma_spent": cloud_cost,
        "client_balance": client_balance
    })))
}

async fn dashboard(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let total_nodes = state.nodes.len();
    let mut total_ram: u64 = 0;
    let mut total_vram: u64 = 0;

    let mut peer_list = Vec::new();
    for entry in state.nodes.iter() {
        let p = &entry.value().info;
        let id = p.node_id.to_hex();
        let k = state.karma.get(&id).map(|v| *v).unwrap_or(0);
        total_ram += p.available_memory_bytes;
        total_vram += p.available_vram_bytes;
        peer_list.push(json!({
            "node_id": id,
            "addr": p.addr.to_string(),
            "tier": format!("{:?}", p.tier),
            "ram_bytes": p.available_memory_bytes,
            "vram_bytes": p.available_vram_bytes,
            "karma": k
        }));
    }

    Json(json!({
        "total_active_nodes": total_nodes,
        "total_network_ram_bytes": total_ram,
        "total_network_vram_bytes": total_vram,
        "nodes": peer_list,
    }))
}
