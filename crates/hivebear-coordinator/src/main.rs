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
        .layer(cors)
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    info!("🚀 HiveBear Coordination Server starting on http://{}", addr);
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

async fn register_node(
    State(state): State<SharedState>,
    Json(info): Json<PeerInfo>,
) -> StatusCode {
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
    state.signals.entry(to_node).or_default().push(val);
    StatusCode::OK
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
    Json(json!({
        "swarm_id": format!("swarm-{}", req.preferred_model.unwrap_or_else(|| "default".to_string())),
        "assigned_role": "worker",
        "total_peers_in_swarm": available_peers,
    }))
}

async fn dashboard(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let total_nodes = state.nodes.len();
    let mut total_ram: u64 = 0;
    let mut total_vram: u64 = 0;

    let mut peer_list = Vec::new();
    for entry in state.nodes.iter() {
        let p = &entry.value().info;
        total_ram += p.available_memory_bytes;
        total_vram += p.available_vram_bytes;
        peer_list.push(json!({
            "node_id": p.node_id.to_hex(),
            "addr": p.addr.to_string(),
            "tier": format!("{:?}", p.tier),
            "ram_bytes": p.available_memory_bytes,
            "vram_bytes": p.available_vram_bytes,
        }));
    }

    Json(json!({
        "total_active_nodes": total_nodes,
        "total_network_ram_bytes": total_ram,
        "total_network_vram_bytes": total_vram,
        "nodes": peer_list,
    }))
}
