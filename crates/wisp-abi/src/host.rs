//! Host HTTP dispatch and routing contracts.

use async_trait::async_trait;

use crate::calls::*;
use crate::result::*;

mod http;
pub use http::{HttpBody, HttpExchange, HttpUpload, DEFAULT_RESPONSE_QUEUE_BYTES};

pub type Result<T> = anyhow::Result<T>;










#[derive(Debug, Clone)]
pub struct PinnedTarget {
    pub url: String,
    pub addr: std::net::SocketAddr,
}

/// Dispatches requests to a locally selected application version.
#[async_trait]
pub trait Dispatcher: Send + Sync + 'static {
    async fn dispatch_http(
        &self,
        target: &RouteTarget,
        req: crate::dispatch::HttpRequest,
    ) -> Result<HttpReply>;

    /// Starts an HTTP response whose body may arrive incrementally. Implementations
    /// without streaming retain their buffered dispatch behavior through this adapter.
    async fn dispatch_http_stream(
        &self,
        target: &RouteTarget,
        req: crate::dispatch::HttpRequest,
    ) -> Result<HttpExchange> {
        self.dispatch_http(target, req)
            .await
            .map(HttpExchange::buffered)
    }

    /// Deliver an owned upload; existing dispatchers receive its complete inline body.
    async fn dispatch_http_upload(
        &self,
        target: &RouteTarget,
        upload: HttpUpload,
    ) -> Result<HttpExchange> {
        self.dispatch_http_stream(target, upload.collect().await?)
            .await
    }



}

/// A resolved route: which app version this request runs against, and under
/// what policy.
#[derive(Debug, Clone)]
pub struct RouteTarget {
    pub app: String,
    pub version: String,
    pub tenant: String,
    pub lifecycle: crate::config::HandlerLifecycle,
    pub tier: CodeTier,
}

/// Which compiled artifact a version is served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTier {
    Fast,
    Optimized,
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The guest returned `done` while still producing body chunks.
    pub streaming: bool,
}
