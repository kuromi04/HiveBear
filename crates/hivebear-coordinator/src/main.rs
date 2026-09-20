use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{delete, get, post};
use axum::Router;
use clap::Parser;
use dashmap::DashMap;
use hivebear_mesh::PeerInfo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;

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
}

struct NodeEntry {
    info: PeerInfo,
    last_seen: Instant,
}

struct CoordinatorState {
    nodes: DashMap<String, NodeEntry>,
    signals: DashMap<String, Vec<serde_json::Value>>,
    karma: DashMap<String, i64>,
    processed_receipts: DashMap<String, Instant>,
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
    let state = Arc::new(CoordinatorState {
        nodes: DashMap::new(),
        signals: DashMap::new(),
        karma: DashMap::new(),
        processed_receipts: DashMap::new(),
    });

    // Background task to prune stale nodes (no heartbeat for 60 seconds)
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
                    // SECURITY FIX: Also clear their signals to prevent memory leak
                    prune_state.signals.remove(id);
                }
                active
            });
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

async fn health_check() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "hivebear-coordinator",
        "version": "0.2.0",
        "patched_by": "@kuromi04",
    }))
}

async fn register_node(State(state): State<SharedState>, Json(info): Json<PeerInfo>) -> StatusCode {
    let node_id = info.node_id.to_hex();
    info!("Registering node {} ({})", node_id, info.addr);
    state.nodes.insert(
        node_id,
        NodeEntry {
            info,
            last_seen: Instant::now(),
        },
    );
    StatusCode::OK
}

async fn heartbeat_node(
    State(state): State<SharedState>,
    Json(info): Json<PeerInfo>,
) -> StatusCode {
    let node_id = info.node_id.to_hex();
    if let Some(mut entry) = state.nodes.get_mut(&node_id) {
        entry.info = info;
        entry.last_seen = Instant::now();
        StatusCode::OK
    } else {
        // Auto-register if missing
        state.nodes.insert(
            node_id,
            NodeEntry {
                info,
                last_seen: Instant::now(),
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
        if info.available_memory_bytes >= min_mem {
            if target_model.is_empty()
                || info
                    .serving_model_id
                    .as_ref()
                    .map(|m| m.contains(&target_model))
                    .unwrap_or(true)
            {
                result.push(info.clone());
            }
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
    Query(query): Query<DeregisterQuery>,
) -> StatusCode {
    if let Some(id) = query.node_id {
        info!("Deregistering node {}", id);
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
    let to_node = signal.to_node.clone();
    let val = serde_json::to_value(signal).unwrap();

    // SECURITY FIX: Cap the number of signals to prevent Memory Leak / DoS
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
    Query(query): Query<PollSignalQuery>,
) -> Json<Vec<serde_json::Value>> {
    if let Some((_, signals)) = state.signals.remove(&query.node_id) {
        Json(signals)
    } else {
        Json(Vec::new())
    }
}

#[derive(Deserialize)]
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

    // Sort peers: prioritizing high-karma contributors first
    let mut ranked_peers: Vec<_> = state
        .nodes
        .iter()
        .map(|entry| {
            let p = &entry.value().info;
            let id = p.node_id.to_hex();
            let peer_karma = state.karma.get(&id).map(|k| *k).unwrap_or(0);
            (id, p.addr.to_string(), peer_karma)
        })
        .collect();

    ranked_peers.sort_by(|a, b| b.2.cmp(&a.2));

    let tier_status = if requester_karma >= 500 {
        "VIP_ALPHA"
    } else if requester_karma > 0 {
        "CONTRIBUTOR"
    } else {
        "COMMUNITY"
    };

    Json(json!({
        "swarm_id": format!("swarm-{}", req.preferred_model.unwrap_or_else(|| "default".to_string())),
        "assigned_role": "worker",
        "total_peers_in_swarm": available_peers,
        "requester_karma": requester_karma,
        "priority_tier": tier_status,
        "ranked_peers": ranked_peers.into_iter().map(|(id, addr, karma)| json!({
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
    Json(json!({
        "node_id": query.node_id,
        "karma": balance,
        "tier": if balance >= 500 { "VIP_ALPHA" } else if balance > 0 { "CONTRIBUTOR" } else { "COMMUNITY" }
    }))
}

#[derive(Deserialize, Serialize, Clone)]
struct WorkReceipt {
    client_node_id: String,
    worker_node_id: String,
    tokens_processed: u64,
    timestamp: u64,
    nonce: String,
    signature_hash: String,
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

    // 1. Anti-Replay verification
    let receipt_key = format!("{}:{}:{}", r.client_node_id, r.nonce, r.timestamp);
    if state.processed_receipts.contains_key(&receipt_key) {
        return Err((
            StatusCode::CONFLICT,
            "Receipt already claimed (anti-replay check failed)".to_string(),
        ));
    }

    // 2. Client verification: check if client exists or has interacted recently
    let client_known = state.nodes.contains_key(&r.client_node_id);
    if !client_known && r.tokens_processed > 5000 {
        return Err((
            StatusCode::FORBIDDEN,
            "Client node not recognized for high-token claims".to_string(),
        ));
    }

    // 3. Rate-limit / Cap per claim: prevent rogue node claiming millions in one shot
    if r.tokens_processed == 0 || r.tokens_processed > 100_000 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid token range in work receipt".to_string(),
        ));
    }

    // 4. Calculate Karma (1 token = 1 Karma point)
    let points = r.tokens_processed as i64;
    let mut current = state.karma.entry(r.worker_node_id.clone()).or_insert(0);
    *current += points;
    let new_balance = *current;

    // Record receipt as processed
    state.processed_receipts.insert(receipt_key, Instant::now());

    info!(
        "🐺 Karma awarded! Worker {} received +{} Karma from client {} (New Balance: {})",
        r.worker_node_id, points, r.client_node_id, new_balance
    );

    Ok(Json(json!({
        "status": "success",
        "worker_node_id": r.worker_node_id,
        "karma_awarded": points,
        "new_balance": new_balance
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
