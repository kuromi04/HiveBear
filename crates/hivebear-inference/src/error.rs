use thiserror::Error;

/// Errors that can occur during inference operations.
#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("Model file not found: {0}")]
    ModelNotFound(String),

    #[error("Unsupported model format: {0}")]
    UnsupportedFormat(String),

    #[error(
        "No usable inference engine for format '{format}' on this platform \
         (this build has no real backend compiled in for it). Update or reinstall HiveBear."
    )]
    NoEngineAvailable { format: String },

    #[error("Engine '{engine}' is not compiled in (missing feature flag)")]
    EngineNotCompiled { engine: String },

    #[error("Failed to load model: {0}")]
    LoadError(String),

    #[error("Generation failed: {0}")]
    GenerationError(String),

    #[error("Invalid model handle (model may have been unloaded)")]
    InvalidHandle,

    #[error("Grammar/schema error: {0}")]
    GrammarError(String),

    #[error("Tool calling error: {0}")]
    ToolCallError(String),

    #[error("Offloading error: {0}")]
    OffloadError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, InferenceError>;
