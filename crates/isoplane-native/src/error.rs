#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("owned process operation failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("local session failed: {0:#}")]
    Session(#[source] anyhow::Error),
    #[error("native input buffer is null or exceeds addressable memory")]
    Buffer,
    #[error("native input text is not UTF-8")]
    Text,
    #[error("the debugger endpoint is not an IP address and port")]
    DebugEndpoint,
    #[error("invalid settings: {0}")]
    Settings(#[from] isoplane_abi::config::InvalidGuestMemory),
    #[error("the runtime cache directory must be an absolute UTF-8 path")]
    CacheDirectory,
    #[error("runtime cache operation failed: {0}")]
    Cache(#[from] isoplane_cache::Error),
    #[error("preparing the shared runtime failed: {0:#}")]
    RuntimePreparation(#[source] anyhow::Error),
}
