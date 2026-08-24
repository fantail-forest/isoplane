//! Host HTTP dispatch and routing contracts.

use async_trait::async_trait;

use crate::calls::*;
use crate::result::*;

pub type Result<T> = anyhow::Result<T>;










/// §8.6 egress/SSRF policy, enforced host-side because the guest cannot be
/// trusted to enforce anything.
pub trait EgressPolicy: Send + Sync + 'static {
    /// Resolve-then-pin, so DNS rebinding cannot move the target between the
    /// check and the connect.
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

// ---------------------------------------------------------------------------
// The proxy <-> cell seam (§9.1, §9.2).
// ---------------------------------------------------------------------------

/// Dispatches requests to a locally selected application version.
#[async_trait]
pub trait Dispatcher: Send + Sync + 'static {
    /// Handler dispatch. Under `reuse` the cell may serve this from a warm
    /// parked instance; under `fresh` it is always a pristine template clone
    /// (§2.6, §6.3).
    async fn dispatch_http(
        &self,
        target: &RouteTarget,
        req: crate::dispatch::HttpRequest,
    ) -> Result<HttpReply>;



}

/// A resolved application version, owner and execution policy.
#[derive(Debug, Clone)]
pub struct RouteTarget {
    pub app: String,
    pub version: String,
    pub tenant: String,
    pub lifecycle: crate::config::HandlerLifecycle,
    /// Compiler tier selected for this route.
    pub tier: CodeTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTier {
    /// Baseline compiler output.
    Fast,
    /// Optimized compiler output.
    Optimized,
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Set when the guest returned `done` with a body-stream handle and is
    /// still producing chunks under `WaitUntil` semantics (§6.3).
    pub streaming: bool,
}
