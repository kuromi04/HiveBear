use crate::error::CmdResult;
use crate::state::AppState;
use hivebear_registry::{metadata::SearchResult, ModelMetadata, StorageReport};
use tauri::State;

#[tauri::command]
pub async fn search_models(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> CmdResult<Vec<SearchResult>> {
    let mut results = state
        .registry
        .search(&query, limit.unwrap_or(20), Some(&state.profile))
        .await
        .map_err(crate::error::CommandError::from)?;

    // On mobile (where only Candle is available), filter out incompatible architectures
    // when browsing (empty query), so mobile users only see models they can actually run.
    let is_mobile = cfg!(target_os = "android") || state.profile.platform.os == "android";
    if is_mobile && query.trim().is_empty() {
        results.retain(|r| r.compatibility_score.unwrap_or(0.0) > 0.0);
    }

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
        if let Some(entry) = builtins.iter().find(|e| e.id == model_id) {
            if !entry.candle_compatible {
                return Err(crate::error::CommandError {
                    message: format!(
                        "Model '{}' cannot run locally on mobile (Candle engine only supports Llama/Mistral architectures). \
                         Please install a Llama model (e.g. Llama 3.2 1B or 3B), or use Mesh to run this model from your PC.",
                        entry.name
                    ),
                    code: crate::error::ErrorCode::RegistryError,
                }.into());
            }
            if entry.params_billions > 3.5 {
                return Err(crate::error::CommandError {
                    message: format!(
                        "Model '{}' ({:.1}B parameters) is too large to run locally on mobile. \
                         Mobile devices support models up to 3.5B (e.g. Llama 3.2 1B or 3B). \
                         You can run this model on your PC and access it from mobile via HiveBear Mesh.",
                        entry.name, entry.params_billions
                    ),
                    code: crate::error::ErrorCode::RegistryError,
                }.into());
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
