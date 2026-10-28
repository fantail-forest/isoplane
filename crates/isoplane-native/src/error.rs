#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("owned process operation failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("local session failed: {0:#}")]
    Session(#[source] anyhow::Error),
    #[error("native input buffer is null or exceeds addressable memory")]
    Buffer,
    #[error("invalid settings: {0}")]
    Settings(#[from] serde_json::Error),
    #[error("the runtime cache directory must be an absolute UTF-8 path")]
    CacheDirectory,
    #[error("runtime cache operation failed: {0}")]
    Cache(#[from] isoplane_cache::Error),
    #[error("preparing the shared runtime failed: {0:#}")]
    RuntimePreparation(#[source] anyhow::Error),
}
