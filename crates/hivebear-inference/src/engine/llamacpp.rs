use async_trait::async_trait;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, Mutex};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::chat_template;
use crate::error::{InferenceError, Result};
use crate::types::*;
use hivebear_core::types::{InferenceEngine, ModelFormat};

use super::{InferenceBackend, TokenStream};

struct SharedBackend(LlamaBackend);
unsafe impl Send for SharedBackend {}
unsafe impl Sync for SharedBackend {}

fn shared_llama_backend() -> Result<&'static LlamaBackend> {
    static BACKEND: std::sync::OnceLock<std::result::Result<SharedBackend, String>> =
        std::sync::OnceLock::new();
    let res = BACKEND.get_or_init(|| {
        let b = LlamaBackend::init().map_err(|e| format!("Failed to init llama backend: {e}"))?;
        llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
        Ok(SharedBackend(b))
    });
    match res {
        Ok(sb) => Ok(&sb.0),
        Err(msg) => Err(InferenceError::LoadError(msg.clone())),
    }
}

/// Internal state for a loaded llama.cpp model + context.
struct LoadedModel {
    model: LlamaModel,
    backend: &'static LlamaBackend,
    config: LoadConfig,
}

// SAFETY: LlamaModel and LlamaBackend are thread-safe in practice —
// llama.cpp uses internal locks. We wrap in Arc<Mutex<>> for Rust's Send/Sync.
unsafe impl Send for LoadedModel {}
unsafe impl Sync for LoadedModel {}

/// llama.cpp inference backend via the `llama-cpp-2` crate.
///
/// Supports GGUF models with GPU offloading, grammar-constrained decoding,
/// and streaming token generation.
pub struct LlamaCppBackend {
    loaded_models: Arc<Mutex<HashMap<u64, Arc<LoadedModel>>>>,
}

impl LlamaCppBackend {
    pub fn new() -> Self {
        Self {
            loaded_models: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for LlamaCppBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl InferenceBackend for LlamaCppBackend {
    fn engine_id(&self) -> InferenceEngine {
        InferenceEngine::LlamaCpp
    }

    fn name(&self) -> &str {
        "llama.cpp"
    }

    fn supported_formats(&self) -> &[ModelFormat] {
        &[ModelFormat::Gguf]
    }

    fn is_available(&self) -> bool {
        true
    }

    fn supports_grammar(&self) -> bool {
        true
    }

    async fn load_model(&self, path: &Path, config: &LoadConfig) -> Result<ModelHandle> {
        let path = path.to_path_buf();
        let config = config.clone();
        let loaded_models = self.loaded_models.clone();

        // Model loading is blocking — run on a blocking thread
        tokio::task::spawn_blocking(move || {
            if !path.exists() {
                return Err(InferenceError::ModelNotFound(path.display().to_string()));
            }

            let backend = shared_llama_backend()?;

            // Set up model params
            let mut model_params = LlamaModelParams::default();
            if config.use_mmap {
                model_params = model_params.with_use_mmap(true);
            }
            if config.use_mlock {
                model_params = model_params.with_use_mlock(true);
            }

            // GPU layer offloading
            let gpu_layers = config.offload.gpu_layers.unwrap_or(if config.offload.auto {
                // Auto: offload as many layers as possible (use 999 to signal "all")
                999
            } else {
                0
            });
            model_params = model_params.with_n_gpu_layers(gpu_layers);

            let model = match LlamaModel::load_from_file(backend, &path, &model_params) {
                Ok(m) => m,
                Err(e) if gpu_layers > 0 => {
                    tracing::warn!("Failed to load model on GPU ({e}), falling back to CPU mode (0 GPU layers)");
                    let mut cpu_params = LlamaModelParams::default();
                    if config.use_mmap {
                        cpu_params = cpu_params.with_use_mmap(true);
                    }
                    if config.use_mlock {
                        cpu_params = cpu_params.with_use_mlock(true);
                    }
                    cpu_params = cpu_params.with_n_gpu_layers(0);
                    LlamaModel::load_from_file(backend, &path, &cpu_params)
                        .map_err(|e2| InferenceError::LoadError(format!("Failed to load model on CPU: {e2}")))?
                }
                Err(e) => return Err(InferenceError::LoadError(format!("Failed to load model: {e}"))),
            };

            tracing::info!(
                path = %path.display(),
                gpu_layers = gpu_layers,
                "Model loaded via llama.cpp"
            );

            let handle = ModelHandle::new(path, InferenceEngine::LlamaCpp);
            let loaded = Arc::new(LoadedModel {
                model,
                backend,
                config: config.clone(),
            });

            loaded_models
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(handle.id, loaded);

            Ok(handle)
        })
        .await
        .map_err(|e| InferenceError::LoadError(format!("Task join error: {e}")))?
    }

    async fn generate(
        &self,
        handle: &ModelHandle,
        req: &GenerateRequest,
    ) -> Result<GenerateResponse> {
        let loaded = self.get_loaded(handle.id)?;
        let req = req.clone();

        tokio::task::spawn_blocking(move || {
            let text = generate_blocking(&loaded, &loaded.config, &req)?;
            Ok(GenerateResponse::Text(text))
        })
        .await
        .map_err(|e| InferenceError::GenerationError(format!("Task join error: {e}")))?
    }

    fn stream(&self, handle: &ModelHandle, req: &GenerateRequest) -> TokenStream {
        let loaded = match self.get_loaded(handle.id) {
            Ok(l) => l,
            Err(e) => {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                tokio::spawn(async move {
                    let _ = tx.send(Err(e)).await;
                });
                return Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx));
            }
        };

        let req = req.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::task::spawn_blocking(move || {
            if let Err(e) = stream_blocking(&loaded, &loaded.config, &req, &tx) {
                let _ = tx.blocking_send(Err(e));
            }
        });

        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }

    async fn unload(&self, handle: &ModelHandle) -> Result<()> {
        self.loaded_models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.id);
        Ok(())
    }
}

impl LlamaCppBackend {
    fn get_loaded(&self, id: u64) -> Result<Arc<LoadedModel>> {
        self.loaded_models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
            .ok_or(InferenceError::InvalidHandle)
    }
}

/// Build the prompt string from chat messages using the correct per-model chat template.
fn build_prompt(req: &GenerateRequest) -> String {
    let model_name = req.model_name.as_deref().unwrap_or("");
    let format = chat_template::detect_template(model_name);
    chat_template::render(format, &req.messages, &req.tools)
}

/// Build the sampler chain from the request's sampling params.
fn build_sampler(model: &LlamaModel, req: &GenerateRequest, seed: u32) -> LlamaSampler {
    let s = &req.sampling;

    let mut samplers: Vec<LlamaSampler> = Vec::new();

    // Penalties (repeat, frequency, presence)
    if s.repeat_penalty != 1.0 || s.frequency_penalty != 0.0 || s.presence_penalty != 0.0 {
        samplers.push(LlamaSampler::penalties(
            64, // look-back window
            s.repeat_penalty,
            s.frequency_penalty,
            s.presence_penalty,
        ));
    }

    // Top-K
    if s.top_k > 0 {
        samplers.push(LlamaSampler::top_k(s.top_k as i32));
    }

    // Top-P (nucleus)
    if s.top_p < 1.0 {
        samplers.push(LlamaSampler::top_p(s.top_p, 1));
    }

    // Temperature
    if s.temperature > 0.0 {
        samplers.push(LlamaSampler::temp(s.temperature));
        samplers.push(LlamaSampler::dist(seed));
    } else {
        samplers.push(LlamaSampler::greedy());
    }

    // Grammar constraint (if provided)
    if let Some(grammar_str) = &req.grammar {
        if let Ok(grammar_sampler) = LlamaSampler::grammar(model, grammar_str, "root") {
            // Insert grammar before the final selection sampler
            samplers.insert(samplers.len().saturating_sub(1), grammar_sampler);
        } else {
            tracing::warn!("Failed to parse GBNF grammar; ignoring grammar constraint");
        }
    }

    LlamaSampler::chain_simple(samplers)
}

/// Tokenize the prompt with automatic sliding-window truncation so multi-turn conversations
/// never fail with `Prompt length exceeds context length` and always leave room for generation.
fn tokenize_with_sliding_window(
    model: &LlamaModel,
    req: &GenerateRequest,
    n_ctx: u32,
) -> Result<Vec<llama_cpp_2::token::LlamaToken>> {
    let reserve = ((n_ctx as usize) / 4).clamp(128, 768);
    let max_prompt = (n_ctx as usize).saturating_sub(reserve).max(64);

    let prompt = build_prompt(req);
    let mut tokens = model
        .str_to_token(&prompt, AddBos::Always)
        .map_err(|e| InferenceError::GenerationError(format!("Tokenization failed: {e}")))?;

    if tokens.is_empty() {
        return Err(InferenceError::GenerationError(
            "Prompt tokenization produced empty tokens".into(),
        ));
    }

    if tokens.len() > max_prompt && req.messages.len() > 1 {
        let mut trimmed_req = req.clone();
        let drop_idx = if matches!(trimmed_req.messages.first(), Some(ChatMessage::System(_))) {
            1
        } else {
            0
        };
        while tokens.len() > max_prompt && trimmed_req.messages.len() > drop_idx + 1 {
            trimmed_req.messages.remove(drop_idx);
            let p = build_prompt(&trimmed_req);
            if let Ok(t) = model.str_to_token(&p, AddBos::Always) {
                if !t.is_empty() {
                    tokens = t;
                }
            }
        }
    }

    if tokens.len() > max_prompt {
        let keep_tail = max_prompt.saturating_sub(1);
        let mut clipped = Vec::with_capacity(max_prompt);
        clipped.push(tokens[0]);
        clipped.extend_from_slice(&tokens[tokens.len() - keep_tail..]);
        tokens = clipped;
    }

    Ok(tokens)
}

/// Blocking generation — runs the full decode loop and returns the complete text.
fn generate_blocking(
    loaded: &LoadedModel,
    config: &LoadConfig,
    req: &GenerateRequest,
) -> Result<String> {
    let max_ctx = config.context_length;
    let tokens = tokenize_with_sliding_window(&loaded.model, req, max_ctx)?;
    let n_ctx = ((tokens.len() as u32) + req.max_tokens.min(1024) + 128).clamp(512, max_ctx);

    let (n_threads_decode, n_threads_batch) = optimal_thread_counts(config.threads);

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(config.batch_size.max(512))
        .with_n_threads(n_threads_decode)
        .with_n_threads_batch(n_threads_batch);

    let mut ctx = loaded
        .model
        .new_context(loaded.backend, ctx_params)
        .map_err(|e| InferenceError::GenerationError(format!("Context creation failed: {e}")))?;

    let seed = config.seed.unwrap_or(42) as u32;
    let mut sampler = build_sampler(&loaded.model, req, seed);

    // Process prompt tokens in chunks matching config.batch_size
    let batch_capacity = (config.batch_size as usize).max(1);
    let mut batch = LlamaBatch::new(batch_capacity, 1);

    for (chunk_idx, chunk) in tokens.chunks(batch_capacity).enumerate() {
        batch.clear();
        let start_pos = chunk_idx * batch_capacity;
        for (i, &token) in chunk.iter().enumerate() {
            let global_pos = (start_pos + i) as i32;
            let is_last = (start_pos + i) == tokens.len() - 1;
            batch
                .add(token, global_pos, &[0], is_last)
                .map_err(|e| InferenceError::GenerationError(format!("Batch add failed: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::GenerationError(format!("Prompt decode failed: {e}")))?;
    }

    let mut output = String::new();
    let eos = loaded.model.token_eos();

    for n_decoded in (tokens.len() as i32..).take(req.max_tokens as usize) {
        if n_decoded >= n_ctx as i32 {
            break;
        }
        let token = sampler.sample(&ctx, -1);
        sampler.accept(token);

        if token == eos || loaded.model.is_eog_token(token) {
            break;
        }

        let piece = token_to_string(&loaded.model, token);
        output.push_str(&piece);

        // Check stop sequences
        if req.stop_sequences.iter().any(|stop| output.ends_with(stop)) {
            for stop in &req.stop_sequences {
                if output.ends_with(stop) {
                    output.truncate(output.len() - stop.len());
                    break;
                }
            }
            break;
        }

        // Prepare next token for decoding
        batch.clear();
        batch
            .add(token, n_decoded, &[0], true)
            .map_err(|e| InferenceError::GenerationError(format!("Batch add failed: {e}")))?;

        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::GenerationError(format!("Decode failed: {e}")))?;
    }

    Ok(output)
}

/// Blocking streaming generation — sends tokens through a channel as they're generated.
fn stream_blocking(
    loaded: &LoadedModel,
    config: &LoadConfig,
    req: &GenerateRequest,
    tx: &tokio::sync::mpsc::Sender<Result<Token>>,
) -> Result<()> {
    let max_ctx = config.context_length;
    let tokens = tokenize_with_sliding_window(&loaded.model, req, max_ctx)?;
    let n_ctx = ((tokens.len() as u32) + req.max_tokens.min(1024) + 128).clamp(512, max_ctx);

    let (n_threads_decode, n_threads_batch) = optimal_thread_counts(config.threads);

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(config.batch_size.max(512))
        .with_n_threads(n_threads_decode)
        .with_n_threads_batch(n_threads_batch);

    let mut ctx = loaded
        .model
        .new_context(loaded.backend, ctx_params)
        .map_err(|e| InferenceError::GenerationError(format!("Context creation failed: {e}")))?;

    let seed = config.seed.unwrap_or(42) as u32;
    let mut sampler = build_sampler(&loaded.model, req, seed);

    // Process prompt tokens in chunks matching config.batch_size
    let batch_capacity = (config.batch_size as usize).max(1);
    let mut batch = LlamaBatch::new(batch_capacity, 1);

    for (chunk_idx, chunk) in tokens.chunks(batch_capacity).enumerate() {
        batch.clear();
        let start_pos = chunk_idx * batch_capacity;
        for (i, &token) in chunk.iter().enumerate() {
            let global_pos = (start_pos + i) as i32;
            let is_last = (start_pos + i) == tokens.len() - 1;
            batch
                .add(token, global_pos, &[0], is_last)
                .map_err(|e| InferenceError::GenerationError(format!("Batch add failed: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::GenerationError(format!("Prompt decode failed: {e}")))?;
    }

    let eos = loaded.model.token_eos();
    let mut accumulated = String::new();

    for n_decoded in (tokens.len() as i32..).take(req.max_tokens as usize) {
        if n_decoded >= n_ctx as i32 {
            break;
        }
        let token = sampler.sample(&ctx, -1);
        sampler.accept(token);

        if token == eos || loaded.model.is_eog_token(token) {
            break;
        }

        let piece = token_to_string(&loaded.model, token);

        accumulated.push_str(&piece);

        // Check stop sequences
        let should_stop = req
            .stop_sequences
            .iter()
            .any(|stop| accumulated.ends_with(stop));

        if should_stop {
            break;
        }

        let tok = Token {
            text: piece,
            id: token.0 as u32,
            logprob: None,
            is_special: false,
        };

        if tx.blocking_send(Ok(tok)).is_err() {
            break; // Receiver dropped
        }

        // Prepare next token
        batch.clear();
        batch
            .add(token, n_decoded, &[0], true)
            .map_err(|e| InferenceError::GenerationError(format!("Batch add failed: {e}")))?;

        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::GenerationError(format!("Decode failed: {e}")))?;
    }

    Ok(())
}

/// Convert a token to a string using the non-deprecated `token_to_piece` API.
fn token_to_string(model: &LlamaModel, token: llama_cpp_2::token::LlamaToken) -> String {
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    model
        .token_to_piece(token, &mut decoder, true, None)
        .unwrap_or_default()
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

/// Choose optimal `(decode_threads, batch_threads)` for `llama.cpp`:
/// - Token decoding (`n_threads`) is memory-bandwidth bound; using physical cores
///   (`logical / 2` on SMT CPUs) avoids hyperthread cache contention (+30–45% tok/s).
/// - Prompt batch prefill (`n_threads_batch`) is compute bound and benefits from all logical cores.
fn optimal_thread_counts(configured: Option<u32>) -> (i32, i32) {
    if let Some(t) = configured {
        let clamped = (t as i32).max(1);
        return (clamped, clamped);
    }
    let logical = num_cpus();
    let decode = if logical >= 4 {
        (logical / 2).clamp(2, 8)
    } else {
        logical.max(1)
    };
    let batch = logical.clamp(1, 16);
    (decode as i32, batch as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llamacpp_backend_metadata() {
        let backend = LlamaCppBackend::new();
        assert_eq!(backend.engine_id(), InferenceEngine::LlamaCpp);
        assert_eq!(backend.name(), "llama.cpp");
        assert!(backend.is_available());
        assert!(backend.supports_grammar());
        assert!(backend.supported_formats().contains(&ModelFormat::Gguf));
    }

    #[test]
    fn test_build_prompt_default_chatml() {
        let req = GenerateRequest {
            messages: vec![
                ChatMessage::System("You are helpful.".into()),
                ChatMessage::user_text("Hi"),
            ],
            ..Default::default()
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("You are helpful."));
        assert!(prompt.contains("Hi"));
        // Default (no model_name) uses ChatML format
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_build_prompt_llama3() {
        let req = GenerateRequest {
            messages: vec![
                ChatMessage::System("You are helpful.".into()),
                ChatMessage::user_text("Hi"),
            ],
            model_name: Some("llama-3.1-8b".into()),
            ..Default::default()
        };
        let prompt = build_prompt(&req);
        assert!(prompt.contains("<|begin_of_text|>"));
        assert!(prompt.contains("You are helpful."));
        assert!(prompt.ends_with("assistant<|end_header_id|>\n\n"));
    }

    #[tokio::test]
    async fn test_load_nonexistent_model() {
        let backend = LlamaCppBackend::new();
        let result = backend
            .load_model(Path::new("/nonexistent/model.gguf"), &LoadConfig::default())
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            InferenceError::ModelNotFound(path) => {
                assert!(path.contains("nonexistent"));
            }
            other => panic!("Expected ModelNotFound, got: {other}"),
        }
    }

    /// End-to-end check of the desktop engine on a real local model: load a small
    /// GGUF through llama.cpp and stream a few tokens.
    ///
    /// Runs only when `HIVEBEAR_TEST_GGUF` points at a directory holding a small
    /// `.gguf` (llama.cpp does not need a sibling tokenizer.json, and nothing is
    /// downloaded here).
    #[tokio::test]
    async fn test_llamacpp_generates_from_real_gguf() {
        use futures::StreamExt;

        let Ok(dir) = std::env::var("HIVEBEAR_TEST_GGUF") else {
            return;
        };
        let dir = Path::new(&dir);
        let gguf = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some("gguf"))
            .unwrap_or_else(|| panic!("no .gguf file in {}", dir.display()));

        let backend = LlamaCppBackend::new();
        let handle = backend
            .load_model(&gguf, &LoadConfig::default())
            .await
            .expect("llama.cpp should load the test GGUF");

        let req = GenerateRequest {
            messages: vec![ChatMessage::user_text("Say hello")],
            max_tokens: 8,
            ..Default::default()
        };

        let mut stream = backend.stream(&handle, &req);
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(tok) => text.push_str(&tok.text),
                Err(e) => panic!("generation failed: {e}"),
            }
        }
        assert!(
            !text.trim().is_empty(),
            "llama.cpp produced an empty stream for a loaded model"
        );
    }
}
