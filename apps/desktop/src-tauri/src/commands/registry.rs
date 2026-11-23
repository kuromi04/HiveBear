use crate::error::CmdResult;
use crate::state::AppState;
use hivebear_registry::{metadata::SearchResult, ModelMetadata, StorageReport};
use tauri::State;

/// Largest model (billions of parameters, Q4-class) a mobile device can hold locally,
/// derived from its total RAM (~0.6 GB per billion params plus runtime overhead,
/// leaving at least ~45% of RAM for Android itself).
fn mobile_max_params_b(total_bytes: u64) -> f64 {
    let total_gb = total_bytes as f64 / 1_073_741_824.0;
    ((total_gb * 0.55 - 0.6) / 0.6).clamp(0.5, 14.0)
}

#[tauri::command]
pub async fn search_models(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> CmdResult<Vec<SearchResult>> {
    let results = state
        .registry
        .search(&query, limit.unwrap_or(20), Some(&state.profile))
        .await
        .map_err(crate::error::CommandError::from)?;

    // Mobile keeps every result: the UI segments them into tiers by device RAM and marks
    // the ones that only run through the PC mesh. Installing is still guarded by RAM below.
    Ok(results)
}

#[tauri::command]
pub async fn install_model(
    state: State<'_, AppState>,
    window: tauri::WebviewWindow,
    model_id: String,
    quant: Option<String>,
) -> CmdResult<hivebear_registry::InstalledInfo> {
    let is_mobile = cfg!(target_os = "android")
        || cfg!(target_os = "ios")
        || state.profile.platform.is_mobile
        || state.profile.platform.os == "android"
        || state.profile.platform.os == "ios";
    if is_mobile {
        let builtins = hivebear_core::recommender::model_db::builtin_models();
        let id_lower = model_id.to_lowercase();
        if let Some(entry) = builtins.iter().find(|e| {
            e.id == model_id
                || e.huggingface_id
                    .as_deref()
                    .and_then(|hf| hf.rsplit('/').next())
                    .map(|slug| slug.to_lowercase() == id_lower)
                    .unwrap_or(false)
        }) {
            if !entry.candle_compatible {
                return Err(crate::error::CommandError {
                    message: format!(
                        "Model '{}' cannot run locally on mobile (Candle engine supports Llama, Qwen2.5, Mistral, SmolLM2, TinyLlama, and Phi-3 architectures). \
                         Please install a compatible model (e.g. Qwen 2.5 0.5B/1.5B or Llama 3.2 1B), or use Mesh to run this model from your PC.",
                        entry.name
                    ),
                    code: crate::error::ErrorCode::RegistryError,
                }
                .into());
            }
            let max_b = mobile_max_params_b(state.profile.memory.total_bytes);
            if entry.params_billions > max_b {
                return Err(crate::error::CommandError {
                    message: format!(
                        "Model '{}' ({:.1}B parameters) is too large to run locally on mobile. \
                         This device ({:.1} GB RAM) supports models up to about {:.1}B parameters. \
                         You can run this model on your PC and access it from mobile via HiveBear Mesh.",
                        entry.name,
                        entry.params_billions,
                        state.profile.memory.total_bytes as f64 / 1_073_741_824.0,
                        max_b
                    ),
                    code: crate::error::ErrorCode::RegistryError,
                }
                .into());
            }
        }
    }

    use tauri::Emitter;
    let progress_cb = move |p: hivebear_registry::download::DownloadProgress| {
        let _ = window.emit("download-progress", &p);
    };
    state
        .registry
        .install(&model_id, quant.as_deref(), None, Some(&progress_cb))
        .await
        .map_err(|e| crate::error::CommandError::from(e).into())
}

#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>, model_id: String) -> CmdResult<bool> {
    Ok(state.registry.cancel_download(&model_id))
}

#[tauri::command]
pub async fn list_installed(state: State<'_, AppState>) -> CmdResult<Vec<ModelMetadata>> {
    Ok(state.registry.list_installed().await)
}

#[tauri::command]
pub async fn remove_model(state: State<'_, AppState>, model_id: String) -> CmdResult<u64> {
    state
        .registry
        .remove(&model_id)
        .await
        .map_err(|e| crate::error::CommandError::from(e).into())
}

#[tauri::command]
pub async fn get_storage_report(state: State<'_, AppState>) -> CmdResult<StorageReport> {
    state
        .registry
        .storage_manager()
        .await
        .report()
        .await
        .map_err(|e| e.to_string())
}
