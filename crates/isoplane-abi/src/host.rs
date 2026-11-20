//! Local routing and HTTP dispatch interfaces consumed by the host.

use async_trait::async_trait;

mod http;
pub use http::{
    HttpBody, HttpExchange, HttpResponseBody, HttpUpload, ResponseDisposition,
    DEFAULT_RESPONSE_QUEUE_BYTES,
};

pub type Result<T> = anyhow::Result<T>;

/// The address an egress check pinned for one outbound fetch.
#[cfg(feature = "rust-guests")]
#[derive(Debug, Clone)]
pub struct PinnedTarget {
    pub url: String,
    pub addr: std::net::SocketAddr,
}

/// Dispatches HTTP requests to the application execution layer.
#[async_trait]
pub trait Dispatcher: Send + Sync + 'static {
    /// Delivers a request with its owned upload and starts the response, whose
    /// body may arrive incrementally.
    async fn dispatch_http_upload(
        &self,
        target: &RouteTarget,
        upload: HttpUpload,
    ) -> Result<HttpExchange>;
}

/// A resolved route: which app version this request dispatch runs against, and under
/// what policy.
#[derive(Debug, Clone)]
pub struct RouteTarget {
    pub app: String,
    pub version: String,
    pub tenant: String,
    pub tier: CodeTier,
}

/// Which compiled artifact a version is served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTier {
    Fast,
    Optimized,
}
