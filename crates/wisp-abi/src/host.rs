//! Host HTTP dispatch and routing contracts.

use async_trait::async_trait;

use crate::calls::*;
use crate::result::*;

pub type Result<T> = anyhow::Result<T>;










/// Egress policy, enforced host-side.
pub trait EgressPolicy: Send + Sync + 'static {
    /// Resolves the target and returns it pinned, so DNS rebinding cannot move
    /// it between the check and the connect.
    fn check(
        &self,
        url: &str,
        allowlist: Option<&[String]>,
    ) -> std::result::Result<PinnedTarget, String>;
}

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
