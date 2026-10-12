mod huggingface;
pub mod local;
pub mod ollama_lib;

use crate::download::{DownloadManager, PartialMeta};
use crate::error::{RegistryError, Result};
use crate::metadata::{InstalledInfo, ModelMetadata, ModelSource, RemoteFile, SearchResult};
use crate::storage::StorageManager;
use chrono::Utc;
use hivebear_core::config::paths::AppPaths;
use hivebear_core::recommender::model_db::ModelCategory;
use hivebear_core::types::{HardwareProfile, ModelFormat};
use hivebear_core::Config;
use huggingface::HuggingFaceSource;
use local::LocalIndex;
use ollama_lib::OllamaSource;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// How deep below a model directory reconciliation looks for GGUF files.
/// HuggingFace repos keep quantizations in subfolders (`UD-Q6_K_XL/…`), so one
/// level is not enough; this stays bounded to keep startup scans cheap.
const MAX_MODEL_SCAN_DEPTH: u8 = 3;

/// A GGUF file found on disk.
#[derive(Debug, Clone)]
struct GgufFile {
    /// Absolute path to the file.
    path: PathBuf,
    /// Path relative to the model directory — what `InstalledInfo.filename` stores.
    relative: String,
    /// File name only, used to detect sharded sets.
    name: String,
    size_bytes: u64,
}

/// What one pass over a model directory found.
#[derive(Debug, Default)]
struct DirScan {
    /// Complete GGUF files (partial downloads are excluded).
    ggufs: Vec<GgufFile>,
    /// Relative names of `<file>.gguf.partial` files still on disk.
    partials: Vec<String>,
}

/// A complete model found under `models_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredModel {
    /// Model ID: the immediate child directory name of `models_dir`.
    id: String,
    /// Directory the model lives in (`InstalledInfo.path`).
    dir: PathBuf,
    /// File to hand to the engine, relative to `dir`.
    filename: String,
    /// Size on disk of the chosen file, or of the whole shard set.
    size_bytes: u64,
}

/// Summary of what [`Registry::reconcile`] changed in the model index.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Model IDs adopted or refreshed from complete files on disk.
    pub adopted: Vec<String>,
    /// Model IDs whose index entry stopped pointing at a real file.
    pub hidden: Vec<String>,
}

impl ReconcileReport {
    /// Whether anything in the index changed.
    pub fn changed(&self) -> bool {
        !self.adopted.is_empty() || !self.hidden.is_empty()
    }
}

/// Join a relative directory prefix with a file name using the platform separator.
fn join_relative(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}{}{name}", std::path::MAIN_SEPARATOR)
    }
}

/// Split a `<prefix>-<NNNNN>-of-<MMMMM>.gguf` name into (prefix, shard index).
fn shard_of(filename: &str) -> Option<(String, u32)> {
    let lower = filename.to_lowercase();
    let stem = lower.strip_suffix(".gguf")?;
    let (head, total) = stem.rsplit_once("-of-")?;
    if total.len() != 5 || !total.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (prefix, index) = head.rsplit_once('-')?;
    if index.len() != 5 || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((prefix.to_string(), index.parse().ok()?))
}

/// Walk `root` (a model directory) collecting complete GGUFs and leftover partials.
async fn scan_model_dir(root: &Path) -> DirScan {
    let mut scan = DirScan::default();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0u8)];

    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();

            if file_type.is_dir() {
                if depth < MAX_MODEL_SCAN_DEPTH && !name.starts_with('.') {
                    stack.push((entry.path(), join_relative(&prefix, &name), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            if let Some(partial) = name.strip_suffix(".partial") {
                scan.partials.push(join_relative(&prefix, partial));
            } else if name.to_lowercase().ends_with(".gguf") {
                let size_bytes = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                scan.ggufs.push(GgufFile {
                    path: entry.path(),
                    relative: join_relative(&prefix, &name),
                    name,
                    size_bytes,
                });
            }
        }
    }

    scan.partials.sort();
    scan
}

/// Choose which file to load: the largest single-file GGUF, or the first shard.
fn pick_gguf(found: &[GgufFile]) -> Option<usize> {
    let unsharded = found
        .iter()
        .enumerate()
        .filter(|(_, file)| shard_of(&file.name).is_none())
        .map(|(index, _)| index);
    if let Some(index) = unsharded.max_by_key(|&index| found[index].size_bytes) {
        return Some(index);
    }

    found
        .iter()
        .enumerate()
        .filter_map(|(index, file)| shard_of(&file.name).map(|(_, shard)| (index, shard)))
        .min_by_key(|(_, shard)| *shard)
        .map(|(index, _)| index)
}

/// Total size of the chosen file, or of its whole shard set.
fn chosen_size(found: &[GgufFile], chosen: usize) -> u64 {
    let file = &found[chosen];
    match shard_of(&file.name) {
        None => file.size_bytes,
        Some((prefix, _)) => found
            .iter()
            .filter(|other| {
                shard_of(&other.name).is_some_and(|(other_prefix, _)| other_prefix == prefix)
            })
            .map(|other| other.size_bytes)
            .sum(),
    }
}

/// Complete models found directly under `models_dir`.
///
/// Directories that only hold `.partial` downloads produce no entry, which is what
/// keeps an interrupted download from showing up as an installed model.
async fn discover_models(models_dir: &Path) -> Vec<DiscoveredModel> {
    let mut discovered = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(models_dir).await else {
        return discovered;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue;
        }

        let scan = scan_model_dir(&entry.path()).await;
        let Some(chosen) = pick_gguf(&scan.ggufs) else {
            continue;
        };
        discovered.push(DiscoveredModel {
            id,
            dir: entry.path(),
            filename: scan.ggufs[chosen].relative.clone(),
            size_bytes: chosen_size(&scan.ggufs, chosen),
        });
    }

    discovered.sort_by(|a, b| a.id.cmp(&b.id));
    discovered
}

/// An interrupted download that can be resumed because its `.partial.meta` survived.
#[derive(Debug, Clone)]
struct InterruptedDownload {
    /// Path of the unfinished file relative to the model directory.
    relative: String,
    /// Metadata recorded when the download started (URL, expected hash, size).
    meta: PartialMeta,
}

/// The most progressed interrupted download under a model directory.
///
/// Returns `None` when nothing is resumable, e.g. a partial whose metadata is
/// unreadable — without a URL there is nothing to resume from.
async fn interrupted_download(model_dir: &Path) -> Option<InterruptedDownload> {
    let mut candidates: Vec<(u64, String)> = Vec::new();
    for relative in scan_model_dir(model_dir).await.partials {
        let size = tokio::fs::metadata(model_dir.join(&relative))
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        candidates.push((size, relative));
    }
    // Resume the attempt that got furthest; name breaks ties deterministically.
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    for (_, relative) in candidates {
        let meta_path = model_dir.join(format!("{relative}.partial.meta"));
        let Ok(contents) = tokio::fs::read_to_string(&meta_path).await else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<PartialMeta>(&contents) else {
            continue;
        };
        if meta.url.is_empty() {
            continue;
        }
        return Some(InterruptedDownload { relative, meta });
    }

    None
}

/// Installed info for a model discovered on disk.
fn installed_info_for(found: &DiscoveredModel, previous: Option<&InstalledInfo>) -> InstalledInfo {
    // Only carry a checksum over when it describes the same file.
    let same_file = previous.is_some_and(|prev| prev.filename == found.filename);
    InstalledInfo {
        path: found.dir.clone(),
        format: ModelFormat::Gguf,
        quantization: huggingface::parse_quantization_from_filename(&found.filename),
        size_bytes: found.size_bytes,
        sha256: if same_file {
            previous.and_then(|prev| prev.sha256.clone())
        } else {
            None
        },
        installed_at: previous
            .map(|prev| prev.installed_at)
            .unwrap_or_else(Utc::now),
        last_used: previous.and_then(|prev| prev.last_used),
        filename: found.filename.clone(),
    }
}

/// Fresh metadata entry for a model discovered on disk.
fn metadata_for_discovered(found: &DiscoveredModel) -> ModelMetadata {
    ModelMetadata {
        id: found.id.clone(),
        name: found.id.clone(),
        params_billions: 0.0,
        formats: vec![ModelFormat::Gguf],
        context_length: 4096,
        quality_score: 0.0,
        category: ModelCategory::General,
        source: ModelSource::Local { imported: true },
        huggingface_id: None,
        installed: None,
        description: None,
        tags: Vec::new(),
        downloads_count: None,
        likes_count: None,
        last_modified: None,
    }
}

/// Whether an entry looks like one reconciliation created from a bare directory,
/// i.e. it has no metadata worth keeping once its file is gone.
fn is_disk_adopted_entry(meta: &ModelMetadata) -> bool {
    matches!(meta.source, ModelSource::Local { .. })
        && meta.huggingface_id.is_none()
        && meta.description.is_none()
        && meta.tags.is_empty()
        && meta.downloads_count.is_none()
        && meta.likes_count.is_none()
}

/// Unified model registry combining all sources.
pub struct Registry {
    hf: HuggingFaceSource,
    ollama: OllamaSource,
    index: Arc<Mutex<LocalIndex>>,
    downloader: DownloadManager,
    models_dir: PathBuf,
}

impl Registry {
    /// Create a new registry from config.
    pub async fn new(config: &Config, paths: &AppPaths) -> Result<Self> {
        paths
            .ensure_dirs()
            .map_err(|e| RegistryError::IndexError(format!("Failed to create directories: {e}")))?;

        let index_path = paths.data_dir.join("registry.json");
        let index = LocalIndex::load(&index_path)?;

        // If config.models_dir is invalid or read-only (e.g. root / or relative on mobile),
        // fallback to paths.models_dir which is guaranteed to be writable
        let models_dir = if config.models_dir.as_os_str().is_empty()
            || config.models_dir == std::path::Path::new(".")
            || !config.models_dir.is_absolute()
            || (cfg!(target_os = "android") && !config.models_dir.starts_with("/data/"))
        {
            paths.models_dir.clone()
        } else {
            config.models_dir.clone()
        };

        let _ = tokio::fs::create_dir_all(&models_dir).await;

        let registry = Self {
            hf: HuggingFaceSource::new(),
            ollama: OllamaSource::new(),
            index: Arc::new(Mutex::new(index)),
            downloader: DownloadManager::new(models_dir.clone()),
            models_dir,
        };

        // Models already on disk but missing from registry.json must be usable
        // without the user re-downloading them.
        if let Err(e) = registry.reconcile().await {
            tracing::warn!("Model index reconciliation at startup failed: {e}");
        }

        Ok(registry)
    }

    /// Bring the model index in line with the complete files on disk.
    ///
    /// An older HiveBear (or a download that finished outside the app) can leave a
    /// complete GGUF under `models_dir` that `registry.json` does not know about —
    /// such a model is invisible and unselectable. This adopts those files, refreshes
    /// entries whose file changed, and hides entries whose file is gone (for example
    /// a directory that now holds only a `.partial`). Idempotent and safe with a
    /// missing or corrupt index.
    pub async fn reconcile(&self) -> Result<ReconcileReport> {
        let discovered = discover_models(&self.models_dir).await;
        let mut index = self.index.lock().await;
        let mut report = ReconcileReport::default();
        let mut changed = false;

        // Adopt or refresh every complete model found on disk.
        for found in &discovered {
            let previous = index
                .find(&found.id)
                .and_then(|meta| meta.installed.clone());

            // Keep an installation that still points at a real file: the user picked
            // that quantization, so disk order must not silently replace it.
            if let Some(info) = previous.as_ref() {
                if tokio::fs::metadata(info.path.join(&info.filename))
                    .await
                    .map(|m| m.is_file())
                    .unwrap_or(false)
                {
                    continue;
                }
            }

            let mut meta = match index.find(&found.id) {
                Some(existing) => existing.clone(),
                None => metadata_for_discovered(found),
            };
            meta.installed = Some(installed_info_for(found, previous.as_ref()));
            if !meta.formats.contains(&ModelFormat::Gguf) {
                meta.formats.push(ModelFormat::Gguf);
            }

            index.upsert_unsaved(meta);
            report.adopted.push(found.id.clone());
            changed = true;
        }

        // Hide entries under models_dir that no longer have a complete file. Models
        // outside models_dir (Ollama blobs, external paths) are none of our business.
        let tracked: Vec<(String, PathBuf, String)> = index
            .all()
            .iter()
            .filter_map(|meta| {
                meta.installed
                    .as_ref()
                    .map(|info| (meta.id.clone(), info.path.clone(), info.filename.clone()))
            })
            .collect();

        for (id, path, filename) in tracked {
            if !path.starts_with(&self.models_dir) {
                continue;
            }
            if discovered.iter().any(|found| found.id == id) {
                continue;
            }
            if tokio::fs::metadata(path.join(&filename))
                .await
                .map(|m| m.is_file())
                .unwrap_or(false)
            {
                continue;
            }

            let Some(existing) = index.find(&id).cloned() else {
                continue;
            };
            if is_disk_adopted_entry(&existing) {
                changed |= index.remove_unsaved(&id);
            } else {
                let mut meta = existing;
                meta.installed = None;
                index.upsert_unsaved(meta);
                changed = true;
            }
            report.hidden.push(id);
        }

        if changed {
            index.save()?;
        }

        Ok(report)
    }

    /// Search for models across all sources.
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        hw_profile: Option<&HardwareProfile>,
    ) -> Result<Vec<SearchResult>> {
        let index = self.index.lock().await;
        let local_results = index.search(query);

        // Search HuggingFace
        let hf_results = match self.hf.search(query, limit).await {
            Ok(results) => results,
            Err(e) => {
                tracing::warn!("HuggingFace search failed: {e}");
                Vec::new()
            }
        };

        drop(index);

        let mut results: Vec<SearchResult> = Vec::new();
        let index = self.index.lock().await;

        // Add local results first
        for meta in local_results {
            let compatibility = hw_profile.map(|hw| compute_compatibility(hw, &meta));
            results.push(SearchResult {
                is_installed: meta.installed.is_some(),
                compatibility_score: compatibility,
                metadata: meta,
            });
        }

        // Add HF results, avoiding duplicates
        for meta in hf_results {
            let already_listed = results.iter().any(|r| r.metadata.id == meta.id);
            if already_listed {
                continue;
            }
            let is_installed = index.find(&meta.id).is_some();
            let compatibility = hw_profile.map(|hw| compute_compatibility(hw, &meta));
            results.push(SearchResult {
                is_installed,
                compatibility_score: compatibility,
                metadata: meta,
            });
        }

        // Sort: installed first, then by compatibility score, then by downloads
        results.sort_by(|a, b| {
            b.is_installed
                .cmp(&a.is_installed)
                .then_with(|| {
                    b.compatibility_score
                        .unwrap_or(0.0)
                        .partial_cmp(&a.compatibility_score.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    b.metadata
                        .downloads_count
                        .unwrap_or(0)
                        .cmp(&a.metadata.downloads_count.unwrap_or(0))
                })
        });

        results.truncate(limit);
        Ok(results)
    }

    /// Cancel an active model download by ID.
    pub fn cancel_download(&self, model_id: &str) -> bool {
        self.downloader.cancel_download(model_id)
    }

    /// Install a model by ID.
    pub async fn install(
        &self,
        model_id: &str,
        quant_preference: Option<&str>,
        file_preference: Option<&str>,
        progress_cb: Option<&(dyn Fn(crate::download::DownloadProgress) + Send + Sync)>,
    ) -> Result<InstalledInfo> {
        // Check if already installed
        {
            let index = self.index.lock().await;
            if let Some(meta) = index.find(model_id) {
                if meta.installed.is_some() {
                    return Err(RegistryError::AlreadyInstalled(model_id.to_string()));
                }
            }
        }

        // Create model subdirectory
        let model_dir = self.models_dir.join(model_id);
        tokio::fs::create_dir_all(&model_dir).await?;

        // A previously interrupted attempt already recorded the exact URL and has
        // part of the file on disk. Resume that URL directly: re-resolving the repo
        // can pick a different file or URL and would throw the progress away.
        if file_preference.is_none() {
            if let Some(interrupted) = interrupted_download(&model_dir).await {
                match self
                    .finish_interrupted_download(model_id, &model_dir, &interrupted, progress_cb)
                    .await
                {
                    Ok(installed) => return Ok(installed),
                    Err(e) => tracing::warn!(
                        "Could not resume the interrupted download of {} for {model_id}: {e}",
                        interrupted.relative
                    ),
                }
            }
        }

        // Resolve HuggingFace repo ID
        let repo_id = self.resolve_hf_repo(model_id).await?;

        // List available files
        let files = self.hf.list_files(&repo_id).await?;
        if files.is_empty() {
            return Err(RegistryError::NoFilesFound(model_id.to_string()));
        }

        // Pick the best file — unless an interrupted download for this model is
        // already on disk, in which case resume *that* file: switching quantization
        // would throw away everything already downloaded.
        let file = match file_preference {
            Some(filename) => pick_file(&files, quant_preference, Some(filename))?,
            None => {
                let interrupted = scan_model_dir(&model_dir).await.partials;
                match interrupted
                    .iter()
                    .find_map(|name| files.iter().find(|f| &f.filename == name).cloned())
                {
                    Some(resuming) => {
                        tracing::info!(
                            "Resuming interrupted download of {} for {model_id}",
                            resuming.filename
                        );
                        resuming
                    }
                    None => pick_file(&files, quant_preference, None)?,
                }
            }
        };

        // Download the model file
        let model_path = self
            .downloader
            .download(
                &file.download_url,
                &model_dir,
                &file.filename,
                file.sha256.as_deref(),
                model_id,
                progress_cb,
            )
            .await?;

        let size_bytes = tokio::fs::metadata(&model_path).await?.len();

        // Candle (the only engine on mobile, and the pure-Rust fallback everywhere
        // else) cannot load a GGUF without a sibling `tokenizer.json`. Best effort:
        // llama.cpp does not need it, so a failure here must not abort the install.
        if self
            .fetch_tokenizer(&repo_id, &model_dir, model_id)
            .await
            .is_none()
        {
            tracing::warn!(
                "Installed {model_id} without tokenizer.json: llama.cpp can still load it, \
                 but the Candle backend (used on Android) will refuse to. It is retried when \
                 the model is loaded."
            );
        }

        // Build installed info
        let installed = InstalledInfo {
            path: model_dir,
            format: ModelFormat::Gguf,
            quantization: file.quantization,
            size_bytes,
            sha256: file.sha256.clone(),
            installed_at: Utc::now(),
            last_used: None,
            filename: file.filename.clone(),
        };

        // Update local index
        let mut index = self.index.lock().await;
        let mut meta = match index.find(model_id) {
            Some(existing) => existing.clone(),
            None => {
                // Create a new metadata entry from what we know
                ModelMetadata {
                    id: model_id.to_string(),
                    name: model_id.to_string(),
                    params_billions: 0.0,
                    formats: vec![ModelFormat::Gguf],
                    context_length: 4096,
                    quality_score: 0.0,
                    category: hivebear_core::recommender::model_db::ModelCategory::General,
                    source: ModelSource::HuggingFace {
                        repo_id: repo_id.clone(),
                        revision: None,
                    },
                    huggingface_id: Some(repo_id),
                    installed: None,
                    description: None,
                    tags: Vec::new(),
                    downloads_count: None,
                    likes_count: None,
                    last_modified: None,
                }
            }
        };
        meta.installed = Some(installed.clone());
        index.add(meta)?;

        Ok(installed)
    }

    /// Finish an interrupted download using the URL recorded in its `.partial.meta`.
    ///
    /// The file on disk decides the URL, not a fresh HuggingFace lookup, so the
    /// bytes already downloaded are kept and the result is adopted into the index.
    async fn finish_interrupted_download(
        &self,
        model_id: &str,
        model_dir: &Path,
        interrupted: &InterruptedDownload,
        progress_cb: Option<&(dyn Fn(crate::download::DownloadProgress) + Send + Sync)>,
    ) -> Result<InstalledInfo> {
        tracing::info!(
            "Resuming interrupted download of {} for {model_id} from {}",
            interrupted.relative,
            interrupted.meta.url
        );

        let model_path = self
            .downloader
            .download(
                &interrupted.meta.url,
                model_dir,
                &interrupted.relative,
                interrupted.meta.sha256_expected.as_deref(),
                model_id,
                progress_cb,
            )
            .await?;

        let size_bytes = tokio::fs::metadata(&model_path).await?.len();
        let installed = InstalledInfo {
            path: model_dir.to_path_buf(),
            format: ModelFormat::Gguf,
            quantization: huggingface::parse_quantization_from_filename(&interrupted.relative),
            size_bytes,
            sha256: interrupted.meta.sha256_expected.clone(),
            installed_at: Utc::now(),
            last_used: None,
            filename: interrupted.relative.clone(),
        };

        let mut index = self.index.lock().await;
        let mut meta = match index.find(model_id) {
            Some(existing) => existing.clone(),
            None => metadata_for_discovered(&DiscoveredModel {
                id: model_id.to_string(),
                dir: model_dir.to_path_buf(),
                filename: interrupted.relative.clone(),
                size_bytes,
            }),
        };
        meta.installed = Some(installed.clone());
        if !meta.formats.contains(&ModelFormat::Gguf) {
            meta.formats.push(ModelFormat::Gguf);
        }
        index.add(meta)?;

        Ok(installed)
    }

    /// Make sure an installed GGUF has its `tokenizer.json` sidecar.
    ///
    /// The Candle backend — the only engine compiled into the mobile builds —
    /// refuses to load a GGUF without this file, so a model installed by an older
    /// HiveBear (which never fetched the tokenizer for `.gguf` files) can only fail
    /// at load time. This is a best-effort repair and never fails the caller's
    /// model load: it returns `None` when the tokenizer is not a GGUF sidecar
    /// problem, is already present, or could not be fetched.
    pub async fn ensure_tokenizer(&self, model_id: &str, model_file: &Path) -> Option<PathBuf> {
        if !model_file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        {
            return None;
        }
        let model_dir = model_file.parent()?;
        let tokenizer_path = model_dir.join("tokenizer.json");
        if tokenizer_path.exists() {
            return Some(tokenizer_path);
        }

        let repo_id = {
            let index = self.index.lock().await;
            index
                .find(model_id)
                .and_then(|meta| meta.huggingface_id.clone())
        };
        let Some(repo_id) = repo_id else {
            tracing::warn!(
                "{} is missing tokenizer.json and has no known HuggingFace repo to fetch it from",
                model_file.display()
            );
            return None;
        };

        tracing::info!(
            "{} is missing tokenizer.json; fetching it for the Candle backend",
            model_file.display()
        );
        self.fetch_tokenizer(&repo_id, model_dir, model_id).await
    }

    /// Fetch `<repo>/tokenizer.json` into `model_dir`, falling back to the repo's
    /// `base_model` (many GGUF repos keep the tokenizer only in the base model).
    ///
    /// Returns the local path when the file is available there.
    async fn fetch_tokenizer(
        &self,
        repo_id: &str,
        model_dir: &Path,
        model_id: &str,
    ) -> Option<PathBuf> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        if tokenizer_path.exists() {
            return Some(tokenizer_path);
        }

        let tokenizer_url = format!("https://huggingface.co/{repo_id}/resolve/main/tokenizer.json");
        match self
            .downloader
            .download(
                &tokenizer_url,
                model_dir,
                "tokenizer.json",
                None,
                model_id,
                None,
            )
            .await
        {
            Ok(path) => {
                tracing::info!("Downloaded tokenizer.json from {repo_id}");
                return Some(path);
            }
            Err(e) => {
                tracing::debug!("{repo_id} has no tokenizer.json at the repo root: {e}");
            }
        }

        let Some(base_repo) = self.base_model_repo(repo_id).await else {
            tracing::warn!(
                "No tokenizer.json for {repo_id}: the repo root has none and the \
                 HuggingFace API reported no base_model to fall back to"
            );
            return None;
        };

        let fallback_url =
            format!("https://huggingface.co/{base_repo}/resolve/main/tokenizer.json");
        match self
            .downloader
            .download(
                &fallback_url,
                model_dir,
                "tokenizer.json",
                None,
                model_id,
                None,
            )
            .await
        {
            Ok(path) => {
                tracing::info!("Downloaded tokenizer.json from base model {base_repo}");
                Some(path)
            }
            Err(e) => {
                tracing::warn!(
                    "Could not fetch tokenizer.json for {repo_id} (tried the repo and \
                     its base model {base_repo}): {e}"
                );
                None
            }
        }
    }

    /// Ask the HuggingFace API which model a (quantized) repo was derived from.
    async fn base_model_repo(&self, repo_id: &str) -> Option<String> {
        let client = reqwest::Client::builder()
            .user_agent("hivebear/0.2.14")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .ok()?;
        let json = client
            .get(format!("https://huggingface.co/api/models/{repo_id}"))
            .send()
            .await
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;

        base_model_from_tags(&json)
    }

    /// Remove an installed model.
    pub async fn remove(&self, model_id: &str) -> Result<u64> {
        let mut index = self.index.lock().await;
        let meta = index
            .find(model_id)
            .ok_or_else(|| RegistryError::ModelNotFound(model_id.to_string()))?
            .clone();

        let freed_bytes = if let Some(ref installed) = meta.installed {
            let size = installed.size_bytes;
            // Remove the model directory
            if installed.path.exists() {
                tokio::fs::remove_dir_all(&installed.path).await?;
            }
            size
        } else {
            0
        };

        index.remove(model_id)?;
        Ok(freed_bytes)
    }

    /// List all installed models.
    ///
    /// Reconciles with disk first so a model that finished downloading (or was
    /// copied in) after startup is visible immediately.
    pub async fn list_installed(&self) -> Vec<ModelMetadata> {
        if let Err(e) = self.reconcile().await {
            tracing::warn!("Model index reconciliation failed: {e}");
        }

        let index = self.index.lock().await;
        index
            .all()
            .iter()
            .filter(|m| m.installed.is_some())
            .cloned()
            .collect()
    }

    /// Resolve a model ID or file path to a local file path.
    pub async fn resolve(&self, model_id_or_path: &str) -> Result<PathBuf> {
        // If it's an existing file path, return it directly
        let path = Path::new(model_id_or_path);
        if path.exists() {
            return Ok(path.to_path_buf());
        }

        // Try to resolve through the local index
        {
            let index = self.index.lock().await;
            if let Some(meta) = index.find(model_id_or_path) {
                if let Some(ref installed) = meta.installed {
                    let model_file = installed.path.join(&installed.filename);
                    if model_file.exists() {
                        return Ok(model_file);
                    }
                }
            }
        }

        // Fall back to the directory of that name under models_dir: this covers a
        // model adopted from disk before the index was next reconciled.
        if let Some(model_file) = self.resolve_in_models_dir(model_id_or_path).await {
            return Ok(model_file);
        }

        Err(RegistryError::ModelNotFound(format!(
            "'{}' is not a file path or installed model. Use `hivebear install {}` first.",
            model_id_or_path, model_id_or_path
        )))
    }

    /// Complete GGUF behind a bare model ID that names a directory under `models_dir`.
    async fn resolve_in_models_dir(&self, model_id: &str) -> Option<PathBuf> {
        // Only a single normal path segment may be used, so `..` and separators
        // cannot escape models_dir.
        let mut components = Path::new(model_id).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return None;
        }

        let scan = scan_model_dir(&self.models_dir.join(model_id)).await;
        let chosen = pick_gguf(&scan.ggufs)?;
        Some(scan.ggufs[chosen].path.clone())
    }

    /// Get metadata for a specific model.
    pub async fn get(&self, model_id: &str) -> Result<Option<ModelMetadata>> {
        // Check local first
        let index = self.index.lock().await;
        if let Some(meta) = index.find(model_id) {
            return Ok(Some(meta.clone()));
        }
        drop(index);

        // Try HuggingFace
        self.hf.get(model_id).await
    }

    /// List available files for a model (for choosing quantization).
    pub async fn list_files(&self, model_id: &str) -> Result<Vec<RemoteFile>> {
        let repo_id = self.resolve_hf_repo(model_id).await?;
        self.hf.list_files(&repo_id).await
    }

    /// Get a reference to the storage manager.
    pub async fn storage_manager(&self) -> StorageManager {
        StorageManager::new(self.models_dir.clone(), self.index.clone())
    }

    /// Check whether Ollama is installed locally.
    pub fn ollama_available(&self) -> bool {
        self.ollama.is_available()
    }

    /// List all models found in the local Ollama installation.
    pub async fn list_ollama(&self) -> Result<Vec<ModelMetadata>> {
        self.ollama.search("", 100).await
    }

    /// Import a model from the local Ollama installation into HiveBear's index.
    pub async fn import_ollama(&self, model_id: &str) -> Result<InstalledInfo> {
        let meta = self
            .ollama
            .get(model_id)
            .await?
            .ok_or_else(|| {
                RegistryError::ModelNotFound(format!(
                    "Ollama model '{}' not found. Run `hivebear import-ollama` to list available models.",
                    model_id
                ))
            })?;

        let installed = meta.installed.clone().ok_or_else(|| {
            RegistryError::ModelNotFound(format!(
                "Ollama model '{}' found in manifests but blob file is missing.",
                model_id
            ))
        })?;

        let mut index = self.index.lock().await;
        index.add(meta)?;

        Ok(installed)
    }

    /// Resolve a model ID to a HuggingFace repo ID.
    async fn resolve_hf_repo(&self, model_id: &str) -> Result<String> {
        // If it already looks like a HF repo (contains '/'), use directly
        if model_id.contains('/') {
            return Ok(model_id.to_string());
        }

        // Check if we have a known HF ID from the builtin DB or local index
        let index = self.index.lock().await;
        if let Some(meta) = index.find(model_id) {
            if let Some(ref hf_id) = meta.huggingface_id {
                return Ok(hf_id.clone());
            }
        }
        drop(index);

        // Check builtin model database
        let builtins = hivebear_core::recommender::model_db::builtin_models();
        if let Some(entry) = builtins.iter().find(|e| e.id == model_id) {
            if let Some(ref hf_id) = entry.huggingface_id {
                return Ok(hf_id.clone());
            }
        }

        // Try searching HuggingFace
        let results = self.hf.search(model_id, 1).await?;
        if let Some(first) = results.first() {
            if let Some(ref hf_id) = first.huggingface_id {
                return Ok(hf_id.clone());
            }
        }

        Err(RegistryError::ModelNotFound(format!(
            "Could not resolve '{}' to a HuggingFace repository",
            model_id
        )))
    }
}

/// Extract the base model repo from a HuggingFace model API response.
///
/// GGUF/quantized repos advertise their source via `base_model:<repo>` tags;
/// adapter and quantized tags are skipped because their tokenizers do not
/// describe this model's vocabulary.
fn base_model_from_tags(json: &serde_json::Value) -> Option<String> {
    json.get("tags")?.as_array()?.iter().find_map(|tag| {
        let base = tag.as_str()?.strip_prefix("base_model:")?;
        if base.starts_with("adapter:") || base.starts_with("quantized:") {
            return None;
        }
        Some(base.to_string())
    })
}

/// Compute a simple compatibility score based on hardware and model size.
fn compute_compatibility(hw: &HardwareProfile, meta: &ModelMetadata) -> f64 {
    // On mobile devices (Android/iOS), Candle is the only available inference engine.
    // Candle's quantized_llama only supports the Llama/Mistral GGUF architecture.
    // Models with other architectures (e.g. Qwen2, Gemma, Phi3) cannot run on-device.
    let is_mobile = hw.platform.os == "android" || hw.platform.os == "ios";
    if is_mobile {
        let is_compatible = hivebear_core::recommender::model_db::builtin_models()
            .iter()
            .find(|e| {
                e.id == meta.id || meta.huggingface_id.as_deref() == e.huggingface_id.as_deref()
            })
            .map(|e| e.candle_compatible)
            .unwrap_or_else(|| {
                let id_lower = meta.id.to_lowercase();
                let name_lower = meta.name.to_lowercase();
                (id_lower.contains("llama") || id_lower.contains("mistral"))
                    && !id_lower.contains("mixtral")
                    && !id_lower.contains("qwen")
                    && !name_lower.contains("qwen")
            });

        if !is_compatible {
            return 0.0;
        }
    }

    let total_memory =
        hw.memory.available_bytes as f64 + hw.gpus.iter().map(|g| g.vram_bytes as f64).sum::<f64>();

    // Rough estimate: Q4_K_M at ~4.5 bits per weight
    let estimated_size = meta.params_billions * 1e9 * 4.5 / 8.0;

    if estimated_size > total_memory {
        0.0
    } else {
        let headroom = 1.0 - (estimated_size / total_memory);
        (headroom * 2.0).clamp(0.0, 1.0)
    }
}

/// Pick the best file from available options based on user preferences.
fn pick_file(
    files: &[RemoteFile],
    quant_preference: Option<&str>,
    file_preference: Option<&str>,
) -> Result<RemoteFile> {
    // If user specified a specific file, find it
    if let Some(filename) = file_preference {
        return files
            .iter()
            .find(|f| f.filename == filename)
            .cloned()
            .ok_or_else(|| {
                RegistryError::NoFilesFound(format!(
                    "File '{}' not found. Available: {}",
                    filename,
                    files
                        .iter()
                        .map(|f| f.filename.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            });
    }

    // If user specified a quantization preference, filter by it
    if let Some(quant_str) = quant_preference {
        let quant_upper = quant_str.to_uppercase().replace('-', "_");
        if let Some(file) = files
            .iter()
            .find(|f| f.filename.to_uppercase().contains(&quant_upper))
        {
            return Ok(file.clone());
        }
        tracing::warn!("Quantization '{quant_str}' not found, using best available");
    }

    // Default: prefer Q4_K_M as a good balance, then Q5_K_M, then largest available
    let preferred_quants = ["Q4_K_M", "Q5_K_M", "Q4_K_S", "Q5_K_S", "Q6_K", "Q8_0"];
    for pref in &preferred_quants {
        if let Some(file) = files
            .iter()
            .find(|f| f.filename.to_uppercase().contains(pref))
        {
            return Ok(file.clone());
        }
    }

    // Fall back to the smallest GGUF file
    files
        .iter()
        .min_by_key(|f| f.size_bytes)
        .cloned()
        .ok_or_else(|| RegistryError::NoFilesFound("No files available".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::RemoteFile;

    #[test]
    fn test_pick_file_by_name() {
        let files = vec![
            RemoteFile {
                filename: "model-Q4_K_M.gguf".into(),
                size_bytes: 4_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q4km".into(),
            },
            RemoteFile {
                filename: "model-Q8_0.gguf".into(),
                size_bytes: 8_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q80".into(),
            },
        ];

        let result = pick_file(&files, None, Some("model-Q8_0.gguf")).unwrap();
        assert_eq!(result.filename, "model-Q8_0.gguf");
    }

    #[test]
    fn test_pick_file_by_quant() {
        let files = vec![
            RemoteFile {
                filename: "model-Q4_K_M.gguf".into(),
                size_bytes: 4_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q4km".into(),
            },
            RemoteFile {
                filename: "model-Q8_0.gguf".into(),
                size_bytes: 8_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q80".into(),
            },
        ];

        let result = pick_file(&files, Some("Q8_0"), None).unwrap();
        assert_eq!(result.filename, "model-Q8_0.gguf");
    }

    #[test]
    fn test_pick_file_default_prefers_q4km() {
        let files = vec![
            RemoteFile {
                filename: "model-Q2_K.gguf".into(),
                size_bytes: 2_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q2k".into(),
            },
            RemoteFile {
                filename: "model-Q4_K_M.gguf".into(),
                size_bytes: 4_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q4km".into(),
            },
            RemoteFile {
                filename: "model-Q8_0.gguf".into(),
                size_bytes: 8_000_000_000,
                sha256: None,
                quantization: None,
                download_url: "https://example.com/q80".into(),
            },
        ];

        let result = pick_file(&files, None, None).unwrap();
        assert_eq!(result.filename, "model-Q4_K_M.gguf");
    }

    #[test]
    fn test_base_model_from_tags_prefers_real_base_model() {
        let json = serde_json::json!({
            "tags": [
                "gguf",
                "base_model:quantized:deepseek-ai/DeepSeek-R1-Distill-Qwen-7B",
                "base_model:deepseek-ai/DeepSeek-R1-Distill-Qwen-7B",
                "base_model:adapter:someone/lora"
            ]
        });
        assert_eq!(
            base_model_from_tags(&json).as_deref(),
            Some("deepseek-ai/DeepSeek-R1-Distill-Qwen-7B")
        );
    }

    #[test]
    fn test_base_model_from_tags_without_base_model() {
        let json = serde_json::json!({ "tags": ["gguf", "text-generation"] });
        assert_eq!(base_model_from_tags(&json), None);
        assert_eq!(base_model_from_tags(&serde_json::json!({})), None);
    }

    // ---- Disk reconciliation -------------------------------------------------

    use tempfile::TempDir;

    fn fixture_paths(tmp: &TempDir) -> AppPaths {
        AppPaths::from_base(tmp.path().to_path_buf())
    }

    async fn registry_for(tmp: &TempDir) -> Registry {
        let paths = fixture_paths(tmp);
        paths.ensure_dirs().unwrap();
        let config = Config {
            models_dir: paths.models_dir.clone(),
            ..Config::default()
        };
        Registry::new(&config, &paths).await.unwrap()
    }

    fn installed_entry(id: &str, dir: &Path, filename: &str, size: u64) -> ModelMetadata {
        ModelMetadata {
            id: id.into(),
            name: id.into(),
            params_billions: 0.0,
            formats: vec![ModelFormat::Gguf],
            context_length: 4096,
            quality_score: 0.0,
            category: ModelCategory::General,
            source: ModelSource::HuggingFace {
                repo_id: "someone/Some-Model-GGUF".into(),
                revision: None,
            },
            huggingface_id: Some("someone/Some-Model-GGUF".into()),
            installed: Some(InstalledInfo {
                path: dir.to_path_buf(),
                format: ModelFormat::Gguf,
                quantization: None,
                size_bytes: size,
                sha256: None,
                installed_at: Utc::now(),
                last_used: None,
                filename: filename.into(),
            }),
            description: None,
            tags: Vec::new(),
            downloads_count: None,
            likes_count: None,
            last_modified: None,
        }
    }

    fn read_index(paths: &AppPaths) -> Vec<ModelMetadata> {
        let contents =
            std::fs::read_to_string(paths.data_dir.join("registry.json")).unwrap_or_default();
        serde_json::from_str(&contents).unwrap_or_default()
    }

    /// (d) A complete GGUF that registry.json does not know about becomes usable.
    #[tokio::test]
    async fn orphan_complete_gguf_is_adopted_and_resolvable() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let model_dir = paths.models_dir.join("qwen2.5-0.5b-instruct-gguf");
        std::fs::create_dir_all(&model_dir).unwrap();
        let gguf = model_dir.join("qwen2.5-0.5b-instruct-q4_k_m.gguf");
        std::fs::write(&gguf, vec![1u8; 4096]).unwrap();

        let registry = registry_for(&tmp).await;

        let installed = registry.list_installed().await;
        let entry = installed
            .iter()
            .find(|m| m.id == "qwen2.5-0.5b-instruct-gguf")
            .expect("the orphan GGUF must be listed");
        let info = entry.installed.as_ref().unwrap();
        assert_eq!(info.filename, "qwen2.5-0.5b-instruct-q4_k_m.gguf");
        assert_eq!(info.size_bytes, 4096);
        assert_eq!(
            registry
                .resolve("qwen2.5-0.5b-instruct-gguf")
                .await
                .unwrap(),
            gguf
        );

        // Idempotent: a second pass must not rewrite the index or duplicate entries.
        let before = std::fs::read_to_string(paths.data_dir.join("registry.json")).unwrap();
        let _ = registry.list_installed().await;
        let after = std::fs::read_to_string(paths.data_dir.join("registry.json")).unwrap();
        assert_eq!(
            before, after,
            "an unchanged reconcile must not rewrite the index"
        );
        assert_eq!(
            read_index(&paths)
                .iter()
                .filter(|m| m.id == "qwen2.5-0.5b-instruct-gguf")
                .count(),
            1,
            "no duplicate entries"
        );
    }

    /// Quantizations kept in a repo subfolder are adopted with a relative filename.
    #[tokio::test]
    async fn nested_sharded_gguf_is_adopted() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let model_dir = paths.models_dir.join("glm-5.3-gguf");
        let nested = model_dir.join("UD-Q6_K_XL");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("GLM-5.3-Flash-UD-Q6_K_XL-00001-of-00002.gguf"),
            vec![2u8; 4096],
        )
        .unwrap();
        std::fs::write(
            nested.join("GLM-5.3-Flash-UD-Q6_K_XL-00002-of-00002.gguf"),
            vec![2u8; 2048],
        )
        .unwrap();

        let registry = registry_for(&tmp).await;
        let installed = registry.list_installed().await;
        let info = installed
            .iter()
            .find(|m| m.id == "glm-5.3-gguf")
            .and_then(|m| m.installed.as_ref())
            .expect("nested GGUF must be adopted");

        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            info.filename,
            format!("UD-Q6_K_XL{sep}GLM-5.3-Flash-UD-Q6_K_XL-00001-of-00002.gguf")
        );
        assert_eq!(info.size_bytes, 4096 + 2048, "shard set size is summed");
        assert_eq!(
            registry.resolve("glm-5.3-gguf").await.unwrap(),
            nested.join("GLM-5.3-Flash-UD-Q6_K_XL-00001-of-00002.gguf")
        );
    }

    /// (e) A directory holding only a `.partial` is not an installed model.
    #[tokio::test]
    async fn partial_only_directory_is_not_listed() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let dir = paths.models_dir.join("qwen2.5-1.5b-instruct-gguf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("qwen2.5-1.5b-instruct-q4_k_m.gguf.partial"),
            vec![7u8; 512],
        )
        .unwrap();
        std::fs::write(
            dir.join("qwen2.5-1.5b-instruct-q4_k_m.gguf.partial.meta"),
            b"{}",
        )
        .unwrap();

        let registry = registry_for(&tmp).await;

        assert!(registry
            .list_installed()
            .await
            .iter()
            .all(|m| m.id != "qwen2.5-1.5b-instruct-gguf"));
        assert!(registry
            .resolve("qwen2.5-1.5b-instruct-gguf")
            .await
            .is_err());
        assert!(
            read_index(&paths).is_empty(),
            "no entry may be created for a partial"
        );
    }

    /// A stale index entry pointing at bytes that are only a `.partial` is hidden.
    #[tokio::test]
    async fn stale_installed_entry_with_only_partial_is_hidden() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let dir = paths.models_dir.join("ornith-1.5-9b-gguf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Ornith-1.5-9B-Q4_K_M.gguf.partial"),
            vec![3u8; 128],
        )
        .unwrap();

        let stale = installed_entry(
            "ornith-1.5-9b-gguf",
            &dir,
            "Ornith-1.5-9B-Q4_K_M.gguf",
            5_000_000_000,
        );
        std::fs::write(
            paths.data_dir.join("registry.json"),
            serde_json::to_string_pretty(&vec![stale]).unwrap(),
        )
        .unwrap();

        let registry = registry_for(&tmp).await;

        assert!(
            registry.list_installed().await.is_empty(),
            "a partial-only directory must not be offered as installed"
        );
        assert!(registry.resolve("ornith-1.5-9b-gguf").await.is_err());
    }

    /// A disk-adopted entry whose directory disappears is cleaned up.
    #[tokio::test]
    async fn vanished_adopted_model_is_removed() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let dir = paths.models_dir.join("local-only-model");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("local-only-model-q4_k_m.gguf"), vec![4u8; 1024]).unwrap();

        let registry = registry_for(&tmp).await;
        assert_eq!(registry.list_installed().await.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(registry.list_installed().await.is_empty());
        assert!(
            read_index(&paths).is_empty(),
            "the stale entry must be gone"
        );
    }

    /// A corrupt registry.json must not break adoption.
    #[tokio::test]
    async fn reconcile_survives_corrupt_index() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.data_dir.join("registry.json"), b"{ this is not json").unwrap();

        let dir = paths.models_dir.join("rescued-model");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rescued-model-q4_k_m.gguf"), vec![5u8; 256]).unwrap();

        let registry = registry_for(&tmp).await;
        assert!(registry
            .list_installed()
            .await
            .iter()
            .any(|m| m.id == "rescued-model"));
    }

    /// `resolve` must not follow `..` out of models_dir.
    #[tokio::test]
    async fn resolve_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let outside = paths.data_dir.join("evil");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("evil.gguf"), vec![6u8; 64]).unwrap();

        let registry = registry_for(&tmp).await;
        assert!(registry.resolve("../evil").await.is_err());
        assert!(registry.resolve("evil/evil.gguf").await.is_err());
    }

    /// A stuck partial is resumed from the URL recorded in its `.partial.meta`,
    /// without re-resolving the repo, and then adopted into the index.
    #[tokio::test]
    async fn install_resumes_stuck_partial_from_recorded_url() {
        use crate::download::test_http::{request_range_offset, response_206, TestServer};

        let tmp = TempDir::new().unwrap();
        let paths = fixture_paths(&tmp);
        paths.ensure_dirs().unwrap();

        let model_id = "qwen2.5-1.5b-instruct-gguf";
        let filename = "qwen2.5-1.5b-instruct-q4_k_m.gguf";
        let model_dir = paths.models_dir.join(model_id);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(
            model_dir.join(format!("{filename}.partial")),
            vec![b'A'; 50],
        )
        .unwrap();

        const TOTAL: u64 = 400;
        let server = TestServer::start(|_, head| {
            let offset = request_range_offset(head);
            response_206(offset, &vec![b'B'; (TOTAL - offset) as usize], TOTAL)
        });
        std::fs::write(
            model_dir.join(format!("{filename}.partial.meta")),
            serde_json::json!({
                "url": server.url,
                "total_bytes": TOTAL,
                "sha256_expected": null,
                "model_id": model_id,
                "filename": filename,
                "started_at": "2026-01-01T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();

        let config = Config {
            models_dir: paths.models_dir.clone(),
            ..Config::default()
        };
        let registry = Registry::new(&config, &paths).await.unwrap();
        let installed = registry.install(model_id, None, None, None).await.unwrap();

        assert_eq!(installed.filename, filename);
        assert_eq!(installed.size_bytes, TOTAL);

        let data = std::fs::read(model_dir.join(filename)).unwrap();
        assert_eq!(&data[..50], &vec![b'A'; 50][..], "progress must be kept");
        assert_eq!(
            &data[50..],
            &vec![b'B'; 350][..],
            "the rest must be fetched"
        );

        // The resume request went straight to the recorded URL: one request, no
        // HuggingFace lookup, starting exactly where the partial stopped.
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].to_lowercase().contains("range: bytes=50-"));

        // It is now visible and resolvable.
        assert!(registry
            .list_installed()
            .await
            .iter()
            .any(|m| m.id == model_id));
        assert!(registry
            .resolve(model_id)
            .await
            .unwrap()
            .ends_with(filename));
    }

    /// Real-machine fixture check (run explicitly; see the task report).
    ///
    /// `HIVEBEAR_FIXTURE_DIR=<copy of %APPDATA%\HiveBear\hivebear> cargo test
    /// -p hivebear-registry -- --ignored real_data_fixture --nocapture`
    #[tokio::test]
    #[ignore = "needs HIVEBEAR_FIXTURE_DIR pointing at a copy of the user's data"]
    async fn real_data_fixture_reconciles_orphan_and_hides_partials() {
        let Ok(fixture) = std::env::var("HIVEBEAR_FIXTURE_DIR") else {
            eprintln!("HIVEBEAR_FIXTURE_DIR is not set; skipping");
            return;
        };
        let paths = AppPaths::from_base(PathBuf::from(fixture));
        let config = Config {
            models_dir: paths.models_dir.clone(),
            ..Config::default()
        };

        let registry = Registry::new(&config, &paths).await.unwrap();
        let installed = registry.list_installed().await;
        let ids: Vec<String> = installed.iter().map(|m| m.id.clone()).collect();
        println!("reconciled installed models ({}): {ids:?}", ids.len());

        // Directories that hold a `.partial` but no complete `.gguf`.
        let mut partial_only = Vec::new();
        for entry in std::fs::read_dir(&paths.models_dir).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let mut stack = vec![entry.path()];
            let (mut has_partial, mut has_complete) = (false, false);
            while let Some(dir) = stack.pop() {
                for item in std::fs::read_dir(&dir).unwrap() {
                    let item = item.unwrap();
                    let name = item.file_name().to_string_lossy().to_string();
                    if item.file_type().unwrap().is_dir() {
                        stack.push(item.path());
                    } else if name.ends_with(".partial") {
                        has_partial = true;
                    } else if name.to_lowercase().ends_with(".gguf") {
                        has_complete = true;
                    }
                }
            }
            if has_partial && !has_complete {
                partial_only.push(entry.file_name().to_string_lossy().to_string());
            }
        }

        println!(
            "partial-only dirs kept hidden ({}): {partial_only:?}",
            partial_only.len()
        );
        assert!(
            !partial_only.is_empty(),
            "fixture should contain stuck downloads"
        );
        for id in &partial_only {
            assert!(
                !ids.contains(id),
                "{id} only has a partial download and must stay hidden"
            );
        }

        for expected in [
            "deepseek-r1-distill-qwen-7b-gguf",
            "qwen2.5-0.5b-instruct-gguf",
        ] {
            assert!(
                ids.contains(&expected.to_string()),
                "expected {expected} to be listed, got {ids:?}"
            );
        }
        let resolved = registry
            .resolve("qwen2.5-0.5b-instruct-gguf")
            .await
            .unwrap();
        println!("resolved orphan -> {}", resolved.display());
        assert!(resolved.ends_with("qwen2.5-0.5b-instruct-q4_k_m.gguf"));
    }
}
