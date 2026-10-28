use crate::error::CmdResult;
use crate::state::AppState;
use hivebear_core::config::MeshConfig;
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct MeshStatus {
    pub enabled: bool,
    pub tier: String,
    pub port: u16,
    pub coordination_server: String,
    pub max_contribution_percent: f64,
    pub min_reputation: f64,
    pub verification_rate: f64,
}

#[tauri::command]
pub fn get_mesh_status(state: State<'_, AppState>) -> CmdResult<MeshStatus> {
    let config = state
        .config
        .lock()
        .map_err(|_| String::from("Config lock poisoned"))?;
    Ok(MeshStatus {
        enabled: config.mesh.enabled,
        tier: config.mesh.tier.clone(),
        port: config.mesh.port,
        // Normalized so the UI and the leaderboard fetch always get a usable
        // absolute URL (no missing scheme, no trailing slash).
        coordination_server: config.mesh.normalized_coordination_server(),
        max_contribution_percent: config.mesh.max_contribution_percent,
        min_reputation: config.mesh.min_reputation,
        verification_rate: config.mesh.verification_rate,
    })
}

#[tauri::command]
pub fn get_mesh_config(state: State<'_, AppState>) -> CmdResult<MeshConfig> {
    let config = state
        .config
        .lock()
        .map_err(|_| String::from("Config lock poisoned"))?;
    let mut mesh = config.mesh.clone();
    // Round-tripping an unnormalized value through save_mesh_config would
    // otherwise persist it again.
    mesh.coordination_server = mesh.normalized_coordination_server();
    Ok(mesh)
}

#[tauri::command]
pub fn save_mesh_config(state: State<'_, AppState>, mut mesh_config: MeshConfig) -> CmdResult<()> {
    // Heal the endpoint instead of persisting a value that would fail later:
    // an empty value falls back to the built-in coordinator, a missing scheme is
    // added, and a trailing slash is removed (it used to turn `/register` into
    // `//register` -> 404).
    mesh_config.coordination_server = mesh_config.normalized_coordination_server();
    let mut config = state
        .config
        .lock()
        .map_err(|_| String::from("Config lock poisoned"))?;
    config.mesh = mesh_config;
    config
        .save()
        .map_err(|e| format!("Failed to save config: {e}"))
}

// ── Mesh lifecycle commands ────────────────────────────────────────

/// Live connection status (vs MeshStatus which is just config).
#[derive(Serialize)]
pub struct MeshConnectionStatus {
    /// Whether the mesh node process is running.
    pub running: bool,
    /// Number of directly connected peers.
    pub peer_count: usize,
    /// The node's hex-encoded public key (if running).
    pub node_id: Option<String>,
    /// Peers the node knows about (discovered; connected or still connecting).
    pub known_peers: usize,
    /// True once the coordination server accepted this node's registration.
    /// `false` while running LAN-only.
    pub registered: bool,
    /// User-actionable reason the mesh is degraded (unreachable coordinator,
    /// rejected registration, transport failure), so the UI never shows a
    /// silent zero-peer state.
    pub last_error: Option<String>,
    /// Normalized coordination-server URL currently in use.
    pub coordinator_url: String,
}

/// Start the mesh node: register with the coordination server and begin heartbeats.
#[tauri::command]
pub async fn join_mesh(state: State<'_, AppState>) -> CmdResult<MeshConnectionStatus> {
    state.start_mesh()?;
    get_mesh_connection_status(state)
}

/// Stop the mesh node: deregister and disconnect.
#[tauri::command]
pub async fn leave_mesh(state: State<'_, AppState>) -> CmdResult<()> {
    state.stop_mesh().await
}

/// Get the live mesh connection status.
#[tauri::command]
pub fn get_mesh_connection_status(state: State<'_, AppState>) -> CmdResult<MeshConnectionStatus> {
    let coordinator_url = {
        let config = state
            .config
            .lock()
            .map_err(|_| String::from("Config lock poisoned"))?;
        config.mesh.normalized_coordination_server()
    };

    let node = state.mesh_node.lock().unwrap_or_else(|e| e.into_inner());
    match node.as_ref() {
        Some(n) => Ok(MeshConnectionStatus {
            running: n.is_running(),
            peer_count: n.peer_count(),
            node_id: Some(n.local_id.to_hex()),
            known_peers: n.known_peer_count(),
            registered: n.is_registered(),
            last_error: n.last_error(),
            coordinator_url,
        }),
        None => Ok(MeshConnectionStatus {
            running: false,
            peer_count: 0,
            node_id: None,
            known_peers: 0,
            registered: false,
            last_error: None,
            coordinator_url,
        }),
    }
}

/// Connect directly to a mesh peer node to offload inference.
#[tauri::command]
pub async fn connect_mesh_peer(
    state: State<'_, AppState>,
    peer_addr: String,
    model_name: Option<String>,
) -> CmdResult<crate::commands::inference::LoadedModel> {
    let addr_str = if peer_addr.contains(':') {
        peer_addr.clone()
    } else {
        format!("{peer_addr}:7878")
    };

    let sa: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => match tokio::net::lookup_host(&addr_str).await {
            Ok(mut addrs) => match addrs.next() {
                Some(a) => a,
                None => return Err(format!("Could not resolve peer address '{}'", peer_addr)),
            },
            Err(e) => return Err(format!("Invalid peer address '{}': {e}", peer_addr)),
        },
    };

    let handle_id = 0x8000_0000_0000_0000 | (rand::random::<u32>() as u64);
    let name = model_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Remote Model".into());

    let session = crate::state::MeshPeerSession {
        handle_id,
        peer_addr: sa,
        model_name: name.clone(),
    };

    {
        let mut sessions = state
            .mesh_peer_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        sessions.insert(handle_id, session);
    }

    Ok(crate::commands::inference::LoadedModel {
        handle_id,
        model_path: format!("Mesh: {} ({})", name, sa),
        engine: "Mesh".into(),
    })
}
