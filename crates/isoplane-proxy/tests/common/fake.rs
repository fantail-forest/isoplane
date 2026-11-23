//! HTTP dispatcher double for proxy tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use async_trait::async_trait;

use isoplane_abi::host::{
    CodeTier, Dispatcher, HttpBody, HttpExchange, HttpResponseBody, HttpUpload,
    ResponseDisposition, RouteTarget,
};
use parking_lot::Mutex;

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
    pub fn new() -> Self {
        FakeCell {
            http_dispatches: AtomicU64::new(0),
            breaker_tripped: Mutex::new(HashMap::new()),
        }
    }

    pub fn http_dispatches(&self) -> u64 {
        self.http_dispatches.load(Relaxed)
    }
    pub fn trip_breaker(&self, route: &str) {
        self.breaker_tripped.lock().insert(route.to_string(), true);
    }
    pub fn reset_breaker(&self, route: &str) {
        self.breaker_tripped.lock().insert(route.to_string(), false);
    }
    pub fn breaker_is_tripped(&self, route: &str) -> bool {
        *self.breaker_tripped.lock().get(route).unwrap_or(&false)
    }
}

#[async_trait]
impl Dispatcher for FakeCell {
    async fn dispatch_http_upload(
        &self,
        target: &RouteTarget,
        mut upload: HttpUpload,
    ) -> anyhow::Result<HttpExchange> {
        let req = upload.request;
        let mut uploaded = Vec::new();
        while let Some(chunk) = upload.body.read().await? {
            uploaded.extend(chunk);
        }
        if self.breaker_is_tripped(&format!("{}:{}", target.app, req.path)) {
            return Ok(complete(
                503,
                vec![("retry-after".into(), "5".into())],
                b"hang breaker tripped".to_vec(),
            ));
        }
        self.http_dispatches.fetch_add(1, Relaxed);
        let body = serde_json::json!({
            "app": target.app,
            "version": target.version,
            "tenant": target.tenant,
            "tier": match target.tier { CodeTier::Fast => "fast", CodeTier::Optimized => "optimized" },
            "method": req.method,
            "path": req.path,
            "headers": req.headers,
            "body": uploaded,
        });
        Ok(complete(
            200,
            vec![
                ("content-type".into(), "application/json".into()),
                ("x-isoplane-version".into(), target.version.clone()),
            ],
            body.to_string().into_bytes(),
        ))
    }
}

fn complete(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> HttpExchange {
    HttpExchange {
        status,
        headers,
        body: Box::new(CompleteBody(Some(body))),
    }
}

/// A response body that was complete before the proxy saw its head.
struct CompleteBody(Option<Vec<u8>>);

#[async_trait]
impl HttpBody for CompleteBody {
    fn remaining_bytes(&self) -> Option<u64> {
        Some(self.0.as_ref().map_or(0, |body| body.len() as u64))
    }

    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.0.take().filter(|body| !body.is_empty()))
    }
}

#[async_trait]
impl HttpResponseBody for CompleteBody {
    async fn finish(self: Box<Self>, _: ResponseDisposition) -> anyhow::Result<()> {
        Ok(())
    }
}
