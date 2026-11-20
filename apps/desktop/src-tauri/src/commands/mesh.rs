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
    let enabled = mesh_config.enabled;
    {
        let mut config = state
            .config
            .lock()
            .map_err(|_| String::from("Config lock poisoned"))?;
        config.mesh = mesh_config;
        config
            .save()
            .map_err(|e| format!("Failed to save config: {e}"))?;
    }
    if enabled {
        let _ = state.start_mesh();
    }
    Ok(())
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

#[derive(Serialize, Clone)]
pub struct DiscoveredMeshPeer {
    pub node_id: String,
    pub addr: String,
    pub external_addr: Option<String>,
    pub serving_model_id: Option<String>,
    pub ram_bytes: u64,
    pub vram_bytes: u64,
    pub is_lan: bool,
    pub source: String,
}

/// Probe a remote peer over QUIC to verify reachability and detect its active/installed model name.
async fn probe_peer_model(
    state: &AppState,
    peer_addr: std::net::SocketAddr,
    timeout_ms: u64,
) -> Result<Option<String>, String> {
    use std::sync::Arc;
    use std::time::Duration;

    let identity_path = state.paths.data_dir.join("node_identity.key");
    let identity = hivebear_mesh::NodeIdentity::load_or_generate(&identity_path)
        .unwrap_or_else(|_| hivebear_mesh::NodeIdentity::generate());
    let security_mode = hivebear_mesh::MeshSecurityMode::default();
    let transport: Arc<dyn hivebear_mesh::transport::MeshTransport> =
        Arc::new(hivebear_mesh::transport::quic::QuicTransport::new(
            identity.node_id.clone(),
            security_mode,
            None,
        ));

    let peer_id = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        transport.connect(peer_addr),
    )
    .await
    .map_err(|_| {
        format!(
            "Tiempo de espera agotado al conectar con {peer_addr}. Verifica que tu PC y móvil estén en el mismo WiFi y que HiveBear tenga 'P2P Mesh' activo en la PC."
        )
    })?
    .map_err(|e| {
        format!(
            "No se pudo conectar al nodo en {peer_addr}: {e}. Verifica que HiveBear esté ejecutándose en la PC con Mesh activado."
        )
    })?;

    let req_msg = hivebear_mesh::transport::protocol::MeshMessage::InferenceRequest {
        session_id: uuid::Uuid::new_v4(),
        model_id: "__probe__".into(),
        messages_json: "[]".into(),
        max_tokens: 0,
        temperature: 0.0,
        top_p: 1.0,
    };

    transport
        .send(&peer_id, req_msg)
        .await
        .map_err(|e| format!("Error al enviar sonda al nodo {peer_addr}: {e}"))?;

    let mut detected_model: Option<String> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

    while let Ok(recv_res) = tokio::time::timeout_at(deadline, transport.recv()).await {
        match recv_res {
            Ok((
                _from,
                hivebear_mesh::transport::protocol::MeshMessage::InferenceToken {
                    text,
                    is_done,
                    ..
                },
            )) => {
                if !text.trim().is_empty() {
                    detected_model = Some(text.trim().to_string());
                }
                if is_done {
                    break;
                }
            }
            Ok((
                _from,
                hivebear_mesh::transport::protocol::MeshMessage::InferenceComplete {
                    error, ..
                },
            )) => {
                if let Some(err) = error {
                    return Err(err);
                }
                break;
            }
            Ok((_from, hivebear_mesh::transport::protocol::MeshMessage::Error { message, .. })) => {
                return Err(message);
            }
            Err(_) => break,
            _ => {}
        }
    }

    let _ = transport.disconnect(&peer_id).await;
    Ok(detected_model)
}

/// Discover active P2P Mesh peers on LAN (UDP broadcast) and Coordinator, including their served models.
#[tauri::command]
pub async fn discover_mesh_peers(state: State<'_, AppState>) -> CmdResult<Vec<DiscoveredMeshPeer>> {
    // Ensure local mesh node is running so LanDiscovery (UDP 7877) is active
    let _ = state.start_mesh();

    let node_opt = {
        let guard = state.mesh_node.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };

    let Some(node) = node_opt else {
        return Ok(Vec::new());
    };

    let mut peer_map: std::collections::HashMap<String, hivebear_mesh::PeerInfo> =
        std::collections::HashMap::new();

    for entry in node.peers.iter() {
        let (info, _) = entry.value();
        if info.node_id != node.local_id {
            peer_map.insert(info.node_id.to_hex(), info.clone());
        }
    }

    if let Ok(found) = node.discovery.find_peers("", 0).await {
        for p in found {
            if p.node_id != node.local_id {
                peer_map.entry(p.node_id.to_hex()).or_insert(p);
            }
        }
    }

    // Active LAN probe fallback if UDP broadcast did not discover any LAN peer
    let has_lan = peer_map.values().any(|p| match p.addr.ip() {
        std::net::IpAddr::V4(v4) => v4.is_private() && !v4.is_loopback(),
        _ => false,
    });

    if !has_lan {
        let mut candidates = Vec::new();
        // Priority target: user's PC address
        if let Ok(pc_addr) = "192.168.0.4:7878".parse::<std::net::SocketAddr>() {
            candidates.push(pc_addr);
        }
        if let Some(std::net::IpAddr::V4(local_v4)) = hivebear_mesh::nat::discover_local_ip() {
            let oct = local_v4.octets();
            for host in [4, 2, 3, 5, 10, 100] {
                if host != oct[3] {
                    let addr = std::net::SocketAddr::new(
                        std::net::IpAddr::V4(std::net::Ipv4Addr::new(oct[0], oct[1], oct[2], host)),
                        7878,
                    );
                    if !candidates.contains(&addr) {
                        candidates.push(addr);
                    }
                }
            }
        }
        for cand in candidates {
            if let Ok(model_opt) = probe_peer_model(&state, cand, 350).await {
                let dummy_id = hivebear_mesh::NodeIdentity::generate().node_id;
                let peer_info = hivebear_mesh::PeerInfo {
                    node_id: dummy_id.clone(),
                    hardware: hivebear_core::profile(),
                    available_memory_bytes: 8 * 1024 * 1024 * 1024,
                    available_vram_bytes: 0,
                    network_bandwidth_mbps: 100.0,
                    latency_ms: None,
                    tier: hivebear_mesh::MeshTier::Free,
                    reputation_score: 1.0,
                    addr: cand,
                    external_addr: None,
                    nat_type: hivebear_mesh::NatType::Unknown,
                    latency_map: std::collections::HashMap::new(),
                    serving_model_id: model_opt.or(Some("Modelo en PC".into())),
                    swarm_id: None,
                    draft_capability: None,
                };
                peer_map.insert(dummy_id.to_hex(), peer_info);
                break;
            }
        }
    }

    let mut list = Vec::new();
    for (hex_id, mut info) in peer_map {
        let is_lan = match info.addr.ip() {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        };
        if info.serving_model_id.is_none() {
            if let Ok(Some(model)) = probe_peer_model(&state, info.addr, 1500).await {
                info.serving_model_id = Some(model);
            }
        }
        let source = if is_lan {
            "LAN WiFi".to_string()
        } else {
            "Coordinator".to_string()
        };
        list.push(DiscoveredMeshPeer {
            node_id: hex_id,
            addr: info.addr.to_string(),
            external_addr: info.external_addr.map(|a| a.to_string()),
            serving_model_id: info.serving_model_id,
            ram_bytes: info.available_memory_bytes,
            vram_bytes: info.available_vram_bytes,
            is_lan,
            source,
        });
    }

    // Sort LAN peers and peers with known models first
    list.sort_by(|a, b| {
        b.serving_model_id
            .is_some()
            .cmp(&a.serving_model_id.is_some())
            .then_with(|| b.is_lan.cmp(&a.is_lan))
    });

    Ok(list)
}

static MESH_HANDLE_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(100_000);

/// Connect directly to a mesh peer node to offload inference.
#[tauri::command]
pub async fn connect_mesh_peer(
    state: State<'_, AppState>,
    peer_addr: String,
    model_name: Option<String>,
) -> CmdResult<crate::commands::inference::LoadedModel> {
    let cleaned = peer_addr
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');

    let addr_str = if cleaned.contains(':') {
        cleaned.to_string()
    } else {
        format!("{cleaned}:7878")
    };

    let sa: std::net::SocketAddr = match addr_str.parse() {
        Ok(a) => a,
        Err(_) => match tokio::net::lookup_host(&addr_str).await {
            Ok(mut addrs) => match addrs.next() {
                Some(a) => a,
                None => return Err(format!("No se pudo resolver la dirección '{peer_addr}'")),
            },
            Err(e) => return Err(format!("Dirección de nodo inválida '{peer_addr}': {e}")),
        },
    };

    // Probe the peer to verify QUIC reachability and auto-detect the model running on the PC
    let probed_model = probe_peer_model(&state, sa, 4000).await?;

    let provided = model_name.map(|s| s.trim().to_string()).filter(|s| {
        !s.is_empty()
            && !matches!(
                s.to_lowercase().as_str(),
                "pc remote model" | "remote model" | "remote pc" | "remote_model" | "desconocido"
            )
    });

    let name = probed_model
        .or(provided)
        .unwrap_or_else(|| "Modelo Remoto PC".into());

    // Use a safe counter < 2^53 so JavaScript numbers never lose precision across Tauri invoke
    let handle_id = MESH_HANDLE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
        sessions.clear();
        sessions.insert(handle_id, session);
    }

    Ok(crate::commands::inference::LoadedModel {
        handle_id,
        model_path: format!("Mesh: {} ({})", name, sa),
        engine: "Mesh".into(),
    })
}
