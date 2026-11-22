use hivebear_core::{AppPaths, Config, HardwareProfile};
use hivebear_inference::Orchestrator;
use hivebear_mesh::MeshNode;
use hivebear_persistence::ChatDatabase;
use hivebear_registry::Registry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// An active connection session to a remote mesh peer for inference offloading.
#[derive(Debug, Clone)]
pub struct MeshPeerSession {
    pub handle_id: u64,
    pub peer_addr: std::net::SocketAddr,
    pub model_name: String,
}
use tracing::info;

/// Mesh inference handler for the Desktop/Mobile Tauri application.
///
/// Automatically uses any currently loaded local model in `Orchestrator`, or
/// loads the requested / first installed model from `Registry` on demand when
/// a remote P2P peer (e.g. mobile APK or Termux) requests inference.
pub struct DesktopMeshInferenceHandler {
    pub orchestrator: Arc<Orchestrator>,
    pub registry: Arc<Registry>,
}

impl DesktopMeshInferenceHandler {
    async fn resolve_or_load_handle(
        &self,
        requested_model: &str,
    ) -> Result<(hivebear_inference::ModelHandle, String), String> {
        let req_lower = requested_model.trim().to_lowercase();
        let is_generic = req_lower.is_empty()
            || req_lower == "__probe__"
            || req_lower == "remote model"
            || req_lower == "pc remote model"
            || req_lower == "remote_model"
            || req_lower == "remote pc"
            || req_lower == "modelo remoto pc"
            || req_lower == "desconocido"
            || req_lower == "default";

        // 1. Check already loaded local models
        let loaded = self.orchestrator.loaded_models();
        if !loaded.is_empty() {
            let chosen = if !is_generic {
                loaded
                    .iter()
                    .find(|m| m.model_path.to_lowercase().contains(&req_lower))
                    .unwrap_or(&loaded[0])
            } else {
                &loaded[0]
            };
            let label = std::path::Path::new(&chosen.model_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&chosen.model_path)
                .to_string();
            return Ok((
                hivebear_inference::ModelHandle {
                    id: chosen.handle_id,
                    model_path: chosen.model_path.clone().into(),
                    engine: chosen.engine,
                },
                label,
            ));
        }

        // 2. No model currently loaded in memory: auto-load from installed models in Registry
        let installed = self.registry.list_installed().await;
        if installed.is_empty() {
            return Err(
                "La PC conectada no tiene ningún modelo instalado o cargado en HiveBear. Instala o carga un modelo en la PC primero."
                    .into(),
            );
        }

        let target = if !is_generic {
            installed
                .iter()
                .find(|m| {
                    m.id.to_lowercase().contains(&req_lower)
                        || m.name.to_lowercase().contains(&req_lower)
                })
                .unwrap_or(&installed[0])
        } else {
            &installed[0]
        };

        let resolved = self
            .registry
            .resolve(&target.id)
            .await
            .map_err(|e| format!("Failed to resolve model '{}' on PC: {e}", target.id))?;
        let _ = self.registry.ensure_tokenizer(&target.id, &resolved).await;

        let handle = self
            .orchestrator
            .load(&resolved, &hivebear_inference::LoadConfig::default())
            .await
            .map_err(|e| format!("Failed to load model '{}' on PC: {e}", target.name))?;

        Ok((handle, target.name.clone()))
    }
}

#[async_trait::async_trait]
impl hivebear_mesh::MeshInferenceHandler for DesktopMeshInferenceHandler {
    async fn handle_inference(
        &self,
        model_id: &str,
        messages_json: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> std::result::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = std::result::Result<String, String>> + Send>>,
        String,
    > {
        use futures::StreamExt;

        // Fast probe request: return the detected model name on this PC
        if model_id == "__probe__" || max_tokens == 0 {
            let loaded = self.orchestrator.loaded_models();
            let model_name = if let Some(first) = loaded.first() {
                std::path::Path::new(&first.model_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&first.model_path)
                    .to_string()
            } else {
                let installed = self.registry.list_installed().await;
                if let Some(first) = installed.first() {
                    first.name.clone()
                } else {
                    return Err(
                        "No hay modelos instalados en el nodo PC. Descarga un modelo en la PC primero."
                            .into(),
                    );
                }
            };

            let (tx, rx) = tokio::sync::mpsc::channel(1);
            let _ = tx.send(Ok(model_name)).await;
            return Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)));
        }

        let (handle, _model_label) = self.resolve_or_load_handle(model_id).await?;

        let messages: Vec<hivebear_inference::types::ChatMessage> =
            serde_json::from_str(messages_json)
                .map_err(|e| format!("Invalid messages_json: {e}"))?;

        let req = hivebear_inference::GenerateRequest {
            messages,
            max_tokens,
            sampling: hivebear_inference::SamplingParams {
                temperature,
                ..Default::default()
            },
            model_name: Some(handle.model_path.display().to_string()),
            ..Default::default()
        };

        let mut token_stream = self
            .orchestrator
            .stream(&handle, &req)
            .map_err(|e| format!("Stream error: {e}"))?;

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(item) = token_stream.next().await {
                let mapped = match item {
                    Ok(tok) => Ok(tok.text),
                    Err(e) => Err(e.to_string()),
                };
                if tx.send(mapped).await.is_err() {
                    break;
                }
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

/// Shared application state managed by Tauri.
pub struct AppState {
    pub config: Mutex<Config>,
    pub profile: HardwareProfile,
    pub orchestrator: Arc<Orchestrator>,
    pub registry: Arc<Registry>,
    pub chat_db: ChatDatabase,
    pub paths: AppPaths,
    pub http_client: reqwest::Client,
    pub mesh_node: Mutex<Option<Arc<MeshNode>>>,
    pub mesh_peer_connections: Mutex<HashMap<u64, MeshPeerSession>>,
}

impl AppState {
    pub fn init() -> Self {
        Self::init_with_paths(AppPaths::new())
    }

    /// Initialize with explicit paths — used on Android where the default
    /// `ProjectDirs` paths point to read-only locations.
    pub fn init_with_paths(paths: AppPaths) -> Self {
        paths
            .ensure_dirs()
            .expect("Failed to create app directories");

        let mut config = Config::load_from(&paths.config_file);
        // Ensure config.models_dir uses the explicit valid models_dir from paths,
        // preventing fallback to read-only directories on mobile platforms
        if cfg!(target_os = "android")
            || config.models_dir.as_os_str().is_empty()
            || !config.models_dir.is_absolute()
        {
            config.models_dir = paths.models_dir.clone();
        }

        let profile = hivebear_core::profile();
        let orchestrator = Arc::new(Orchestrator::with_config(profile.clone(), &config));
        let registry = Arc::new(
            tauri::async_runtime::block_on(Registry::new(&config, &paths))
                .expect("Failed to initialize model registry"),
        );
        let chat_db = ChatDatabase::open(&paths.db_file).expect("Failed to open chat database");

        Self {
            config: Mutex::new(config),
            profile,
            orchestrator,
            registry,
            chat_db,
            paths,
            http_client: reqwest::Client::new(),
            mesh_node: Mutex::new(None),
            mesh_peer_connections: Mutex::new(HashMap::new()),
        }
    }

    /// Detect the name of the model this node can serve over P2P Mesh.
    pub fn detect_serving_model_id(&self) -> Option<String> {
        let loaded = self.orchestrator.loaded_models();
        if let Some(first) = loaded.first() {
            return std::path::Path::new(&first.model_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .or_else(|| Some(first.model_path.clone()));
        }
        // Check models_dir on disk for any installed GGUF model directory
        if let Ok(entries) = std::fs::read_dir(&self.paths.models_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    if let Ok(files) = std::fs::read_dir(entry.path()) {
                        let has_gguf = files.flatten().any(|f| {
                            let fname = f.file_name().to_string_lossy().to_lowercase();
                            fname.ends_with(".gguf") && !fname.ends_with(".partial")
                        });
                        if has_gguf {
                            return Some(name);
                        }
                    }
                }
            }
        }
        None
    }

    /// Start the mesh node: register with the coordination server and begin heartbeats.
    ///
    /// Uses the same persistent Ed25519 identity as device-key auth.
    /// Safe to call multiple times — no-ops if already running.
    pub fn start_mesh(&self) -> Result<(), String> {
        {
            let existing = self.mesh_node.lock().unwrap_or_else(|e| e.into_inner());
            if existing.as_ref().is_some_and(|n| n.is_running()) {
                return Ok(());
            }
        }

        let tier;
        let coordination_server;
        let port;
        let lan_discovery;
        let stun_servers;
        let relay_servers;
        let relay_enabled;
        {
            let mut config = self.config.lock().unwrap_or_else(|e| e.into_inner());
            if !config.mesh.enabled {
                config.mesh.enabled = true;
                let _ = config.save();
            }
            tier = hivebear_mesh::MeshTier::from_str_lossy(&config.mesh.tier);
            // Normalized on load; normalize again so a value edited in Settings
            // and saved by an older build cannot reach the HTTP client raw.
            coordination_server = config.mesh.normalized_coordination_server();
            port = config.mesh.port;
            lan_discovery = config.mesh.mdns_enabled;
            stun_servers = config.mesh.stun_servers.clone();
            relay_servers = config.mesh.relay_servers.clone();
            relay_enabled = config.mesh.relay_enabled;
        }

        let identity_path = self.paths.data_dir.join("node_identity.key");
        let identity = hivebear_mesh::NodeIdentity::load_or_generate(&identity_path)
            .map_err(|e| format!("Failed to load identity: {e}"))?;

        let security_mode = hivebear_mesh::MeshSecurityMode::default();
        let transport: Arc<dyn hivebear_mesh::MeshTransport> =
            Arc::new(hivebear_mesh::transport::quic::QuicTransport::new(
                identity.node_id.clone(),
                security_mode,
                None,
            ));

        // Primary source: the coordination server.
        let worker_coordinator_url = coordination_server.clone();
        let primary = Arc::new(
            hivebear_mesh::discovery::server::CoordinationServerClient::new(coordination_server),
        );
        let mut discovery = hivebear_mesh::discovery::composite::CompositeDiscovery::new(primary);

        // LAN fallback: two nodes on the same network find each other over UDP
        // broadcast even when no coordination server is reachable.
        if lan_discovery {
            discovery.add_source(Arc::new(hivebear_mesh::discovery::lan::LanDiscovery::new(
                hivebear_mesh::discovery::LAN_DISCOVERY_PORT,
            )));
        }
        let discovery: Arc<dyn hivebear_mesh::discovery::PeerDiscovery> = Arc::new(discovery);

        let reputation_path = Some(self.paths.data_dir.join("reputation.json"));
        let mut node =
            MeshNode::with_identity(identity, transport, discovery, tier, reputation_path);
        // Apply the configured NAT/relay settings: these fields were dead config
        // and the node used hardcoded values (including an unresolvable relay).
        node.stun_servers = stun_servers;
        node.relay_servers = if relay_enabled {
            relay_servers
        } else {
            Vec::new()
        };
        let node = Arc::new(node);

        let listen_addr: std::net::SocketAddr = format!("0.0.0.0:{}", port)
            .parse()
            .map_err(|e| format!("Invalid mesh port {}: {e}", port))?;
        let total_vram: u64 = self.profile.gpus.iter().map(|g| g.vram_bytes).sum();

        let mut advertise_addr = listen_addr;
        if let Some(ip) = hivebear_mesh::nat::discover_local_ip() {
            advertise_addr.set_ip(ip);
        }

        let serving_model_id = self.detect_serving_model_id();

        let local_info = hivebear_mesh::PeerInfo {
            node_id: node.local_id.clone(),
            hardware: self.profile.clone(),
            available_memory_bytes: self.profile.memory.available_bytes,
            available_vram_bytes: total_vram,
            network_bandwidth_mbps: 100.0,
            latency_ms: None,
            tier,
            reputation_score: 1.0,
            addr: advertise_addr,
            external_addr: None,
            nat_type: hivebear_mesh::NatType::Unknown,
            latency_map: std::collections::HashMap::new(),
            serving_model_id,
            swarm_id: None,
            draft_capability: None,
        };

        // Spawn MeshWorkerDaemon so this node answers incoming P2P InferenceRequest messages
        let handler = Arc::new(DesktopMeshInferenceHandler {
            orchestrator: self.orchestrator.clone(),
            registry: self.registry.clone(),
        });
        let worker_transport = node.transport.clone();
        tauri::async_runtime::spawn(async move {
            let daemon = hivebear_mesh::MeshWorkerDaemon::new(handler, worker_transport)
                .with_coordinator(worker_coordinator_url);
            daemon.run().await;
        });

        // Start in background — never blocks the caller
        node.start_background(listen_addr, local_info);
        info!("Mesh node and worker daemon starting in background");

        *self.mesh_node.lock().unwrap_or_else(|e| e.into_inner()) = Some(node);
        Ok(())
    }

    /// Stop the mesh node gracefully: deregister and disconnect all peers.
    pub async fn stop_mesh(&self) -> Result<(), String> {
        let node = self
            .mesh_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(node) = node {
            node.stop()
                .await
                .map_err(|e| format!("Failed to stop mesh: {e}"))?;
            info!("Mesh node stopped");
        }
        Ok(())
    }

    /// Create AppPaths from a base directory (e.g., Tauri's app_data_dir).
    pub fn paths_from_base(base: PathBuf) -> AppPaths {
        AppPaths {
            config_dir: base.join("config"),
            config_file: base.join("config").join("config.toml"),
            data_dir: base.join("data"),
            models_dir: base.join("data").join("models"),
            db_file: base.join("data").join("hivebear.db"),
            cache_dir: base.join("cache"),
            benchmark_cache: base.join("cache").join("benchmark.json"),
        }
    }
}
