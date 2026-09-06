//! HTTP dispatcher double for proxy tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use async_trait::async_trait;
use parking_lot::Mutex;
use wisp_abi::config::HandlerLifecycle;
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{CodeTier, Dispatcher, HttpReply, RouteTarget};

/// HTTP dispatcher double for proxy boundary tests.
pub struct FakeCell {
    http_dispatches: AtomicU64,
    breaker_tripped: Mutex<HashMap<String, bool>>,
}

impl Default for FakeCell {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCell {
    /// Construct an untripped dispatcher with no accepted requests.
    pub fn new() -> Self {
        FakeCell {
            http_dispatches: AtomicU64::new(0),
            breaker_tripped: Mutex::new(HashMap::new()),
        }
    }

    /// Return the accepted HTTP dispatch count.
    pub fn http_dispatches(&self) -> u64 {
        self.http_dispatches.load(Relaxed)
    }

    /// Make requests to this application/path key return the breaker response.
    pub fn trip_breaker(&self, route: &str) {
        self.breaker_tripped.lock().insert(route.to_string(), true);
    }

    /// Allow requests to this application/path key again.
    pub fn reset_breaker(&self, route: &str) {
        self.breaker_tripped.lock().insert(route.to_string(), false);
    }

    /// Return whether this application/path key is tripped.
    pub fn breaker_is_tripped(&self, route: &str) -> bool {
        *self.breaker_tripped.lock().get(route).unwrap_or(&false)
    }
}

#[async_trait]
impl Dispatcher for FakeCell {
    async fn dispatch_http(
        &self,
        target: &RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        if self.breaker_is_tripped(&format!("{}:{}", target.app, req.path)) {
            return Ok(HttpReply {
                status: 503,
                headers: vec![("retry-after".into(), "5".into())],
                body: b"hang breaker tripped".to_vec(),
                streaming: false,
            });
        }
        self.http_dispatches.fetch_add(1, Relaxed);
        let body = serde_json::json!({
            "app": target.app,
            "version": target.version,
            "tenant": target.tenant,
            "tier": match target.tier { CodeTier::Fast => "fast", CodeTier::Optimized => "optimized" },
            "lifecycle": match target.lifecycle { HandlerLifecycle::Reuse => "reuse", HandlerLifecycle::Fresh => "fresh" },
            "method": req.method,
            "path": req.path,
            "headers": req.headers,
            "body": req.body,
        });
        Ok(HttpReply {
            status: 200,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-wisp-version".into(), target.version.clone()),
            ],
            body: body.to_string().into_bytes(),
            streaming: false,
        })
    }
}
