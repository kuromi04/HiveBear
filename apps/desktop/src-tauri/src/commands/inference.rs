use crate::error::CmdResult;
use crate::state::AppState;
use hivebear_inference::types::{ChatMessage, ModelInfo};
use serde::Serialize;
use tauri::State;

#[derive(Serialize)]
pub struct LoadedModel {
    pub handle_id: u64,
    pub model_path: String,
    pub engine: String,
}

#[tauri::command]
pub async fn load_model(
    state: State<'_, AppState>,
    model_path: String,
    context_length: Option<u32>,
    gpu_layers: Option<u32>,
) -> CmdResult<LoadedModel> {
    let mut load_config = hivebear_inference::LoadConfig::default();
    if let Some(ctx) = context_length {
        load_config.context_length = ctx;
    }
    if let Some(layers) = gpu_layers {
        load_config.offload.gpu_layers = Some(layers);
        load_config.offload.auto = false;
    }

    let resolved = state
        .registry
        .resolve(&model_path)
        .await
        .map_err(|e| format!("Failed to resolve model '{}': {e}", model_path))?;

    // The Candle backend (the only engine compiled into mobile builds) needs a
    // `tokenizer.json` next to the GGUF. Installs made before HiveBear fetched it
    // never created that file, which made every mobile load fail. Best effort:
    // engine selection/loading still reports the real error below if it cannot be
    // fetched (e.g. offline).
    let _ = state
        .registry
        .ensure_tokenizer(&model_path, &resolved)
        .await;

    let handle = state
        .orchestrator
        .load(&resolved, &load_config)
        .await
        .map_err(|e| String::from(crate::error::CommandError::from(e)))?;

    Ok(LoadedModel {
        handle_id: handle.id,
        model_path: handle.model_path.display().to_string(),
        engine: format!("{:?}", handle.engine),
    })
}

#[tauri::command]
pub async fn stream_chat(
    state: State<'_, AppState>,
    window: tauri::WebviewWindow,
    handle_id: u64,
    messages: Vec<ChatMessage>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> CmdResult<String> {
    use futures::StreamExt;
    use tauri::Emitter;

    let mesh_session = {
        let sessions = state
            .mesh_peer_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        sessions.get(&handle_id).cloned()
    };

    if let Some(session) = mesh_session {
        return stream_mesh_peer_chat(&state, window, &session, messages, temperature, max_tokens)
            .await;
    }

    let models = state.orchestrator.loaded_models();
    let info = models
        .iter()
        .find(|m| m.handle_id == handle_id)
        .ok_or_else(|| String::from("Model not loaded"))?;

    let handle = hivebear_inference::ModelHandle {
        id: handle_id,
        model_path: info.model_path.clone().into(),
        engine: info.engine,
    };

    let mut request = hivebear_inference::GenerateRequest {
        messages,
        model_name: Some(info.model_path.clone()),
        ..Default::default()
    };
    if let Some(t) = temperature {
        request.sampling.temperature = t;
    }
    if let Some(m) = max_tokens {
        request.max_tokens = m;
    }

    let mut stream = state
        .orchestrator
        .stream(&handle, &request)
        .map_err(|e| String::from(crate::error::CommandError::from(e)))?;

    let mut full_text = String::new();
    let mut stream_error: Option<String> = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(token) => {
                full_text.push_str(&token.text);
                let _ = window.emit("chat-token", &token.text);
            }
            Err(e) => {
                let message = e.to_string();
                // Still emitted so a partial answer stays visible next to the error.
                let _ = window.emit("chat-error", &message);
                stream_error = Some(message);
                break;
            }
        }
    }

    let _ = window.emit("chat-done", ());

    // A stream that ends with no token and no error (immediate EOS, a backend that
    // produced nothing, a stub engine, ...) used to leave the user staring at an
    // empty assistant bubble with no explanation: fail the command so the UI always
    // shows it. The same applies to an error that produced nothing — the event above
    // can be delivered after the frontend has already unlistened, so the returned
    // error is the reliable channel.
    if full_text.trim().is_empty() {
        return Err(match stream_error {
            Some(message) => message,
            None => format!(
                "The model produced no output (engine: {:?}). Unload and load the model again, \
                 or install a different model.",
                info.engine
            ),
        });
    }

    if let Some(message) = stream_error {
        // The answer is incomplete but non-empty; the `chat-error` event carries the reason.
        tracing::warn!("chat stream ended early for handle {handle_id}: {message}");
    }

    Ok(full_text)
}

#[tauri::command]
pub async fn unload_model(state: State<'_, AppState>, handle_id: u64) -> CmdResult<()> {
    {
        let mut mesh_sessions = state
            .mesh_peer_connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if mesh_sessions.remove(&handle_id).is_some() {
            return Ok(());
        }
    }
    let models = state.orchestrator.loaded_models();
    let info = models
        .iter()
        .find(|m| m.handle_id == handle_id)
        .ok_or_else(|| String::from("Model not loaded"))?;

    let handle = hivebear_inference::ModelHandle {
        id: handle_id,
        model_path: info.model_path.clone().into(),
        engine: info.engine,
    };

    state
        .orchestrator
        .unload(&handle)
        .await
        .map_err(|e| String::from(crate::error::CommandError::from(e)))
}

#[tauri::command]
pub fn list_loaded_models(state: State<'_, AppState>) -> CmdResult<Vec<ModelInfo>> {
    let mut list = state.orchestrator.loaded_models();
    let mesh_sessions = state
        .mesh_peer_connections
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for session in mesh_sessions.values() {
        list.push(ModelInfo {
            handle_id: session.handle_id,
            model_path: format!("Mesh: {} ({})", session.model_name, session.peer_addr),
            engine: hivebear_core::types::InferenceEngine::Mesh,
            context_length: 4096,
            gpu_layers: None,
        });
    }
    Ok(list)
}

async fn stream_mesh_peer_chat(
    state: &AppState,
    window: tauri::WebviewWindow,
    session: &crate::state::MeshPeerSession,
    messages: Vec<ChatMessage>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> CmdResult<String> {
    use std::sync::Arc;
    use tauri::Emitter;

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

    let peer_id = transport.connect(session.peer_addr).await.map_err(|e| {
        format!(
            "Failed to connect to mesh peer at {}: {e}",
            session.peer_addr
        )
    })?;

    let session_id = uuid::Uuid::new_v4();
    let messages_json =
        serde_json::to_string(&messages).map_err(|e| format!("Serialization error: {e}"))?;

    let req_msg = hivebear_mesh::transport::protocol::MeshMessage::InferenceRequest {
        session_id,
        model_id: session.model_name.clone(),
        messages_json,
        max_tokens: max_tokens.unwrap_or(2048),
        temperature: temperature.unwrap_or(0.7),
        top_p: 0.9,
    };

    transport
        .send(&peer_id, req_msg)
        .await
        .map_err(|e| format!("Failed to send inference request to peer: {e}"))?;

    let mut full_text = String::new();
    let mut stream_error: Option<String> = None;

    loop {
        match transport.recv().await {
            Ok((
                _from,
                hivebear_mesh::transport::protocol::MeshMessage::InferenceToken {
                    text,
                    is_done,
                    ..
                },
            )) => {
                if is_done {
                    break;
                }
                full_text.push_str(&text);
                let _ = window.emit("chat-token", &text);
            }
            Ok((
                _from,
                hivebear_mesh::transport::protocol::MeshMessage::InferenceComplete {
                    error, ..
                },
            )) => {
                if let Some(err) = error {
                    let _ = window.emit("chat-error", &err);
                    stream_error = Some(err);
                }
                break;
            }
            Ok((_from, hivebear_mesh::transport::protocol::MeshMessage::Error { message, .. })) => {
                let _ = window.emit("chat-error", &message);
                stream_error = Some(message);
                break;
            }
            Err(e) => {
                let err_str = e.to_string();
                let _ = window.emit("chat-error", &err_str);
                stream_error = Some(err_str);
                break;
            }
            _ => {}
        }
    }

    let _ = window.emit("chat-done", ());

    if full_text.trim().is_empty() {
        return Err(match stream_error {
            Some(err) => err,
            None => format!(
                "The remote mesh peer ({}) returned no output. Verify that the model is loaded on the peer.",
                session.peer_addr
            ),
        });
    }

    Ok(full_text)
}
