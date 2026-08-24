//! HTTP routing, admission and connection lifecycle.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use wisp_abi::dispatch::{HttpRequest, };
use wisp_abi::host::{Dispatcher, RouteTarget};

use crate::metrics::ProxyMetrics;
use crate::routes::RouteResolver;
use crate::shed::{Admit, AdmissionControl, ShedConfig};

pub type Body = Full<Bytes>;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub listen: SocketAddr,
    pub shed: ShedConfig,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            listen: ([127, 0, 0, 1], 8787).into(),
            shed: ShedConfig::default(),
        }
    }
}

pub struct Proxy {
    pub routes: Arc<dyn RouteResolver>,
    pub dispatcher: Arc<dyn Dispatcher>,
    pub metrics: Arc<ProxyMetrics>,
    admission: Arc<AdmissionControl>,
}

impl Proxy {
    pub fn new(
        routes: Arc<dyn RouteResolver>,
        dispatcher: Arc<dyn Dispatcher>,
        metrics: Arc<ProxyMetrics>,
        cfg: ProxyConfig,
    ) -> Arc<Self> {
        Arc::new(Proxy {
            admission: Arc::new(AdmissionControl::new(cfg.shed, metrics.clone())),
            routes,
            dispatcher,
            metrics,
        })
    }



    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        let local = listener.local_addr()?;
        tracing::info!(addr = %local, "proxy listening (plaintext; TLS is out of PoC scope)");
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    continue;
                }
            };
            let me = self.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req| {
                    let me = me.clone();
                    async move { me.handle(req, peer).await }
                });
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await
                {
                    tracing::debug!(error = %e, "connection closed");
                }
            });
        }
    }

    /// One request. Public so tests can drive the proxy without a socket.
    pub async fn handle(
        self: Arc<Self>,
        req: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Response<Body>, hyper::Error> {
        let started = Instant::now();
        self.metrics.requests_total.fetch_add(1, Relaxed);
        self.metrics.route_generation.store(self.routes.generation(), Relaxed);

        let resp = self.route_and_dispatch(req, peer).await;
        self.metrics.request_latency.record(started.elapsed());
        Ok(resp)
    }

    async fn route_and_dispatch(
        self: &Arc<Self>,
        req: Request<Incoming>,
        peer: SocketAddr,
    ) -> Response<Body> {
        let host = req
            .headers()
            .get(hyper::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| req.uri().host().map(str::to_string))
            .unwrap_or_default();
        let path = req.uri().path().to_string();

        // Admission happens before
        // route resolution, because an overloaded node should spend as little
        // as possible on a request it is about to refuse.
        let _permit = match self.admission.admit().await {
            Admit::Granted(p) => p,
            Admit::Shed => {
                return text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "overloaded",
                    &[("retry-after", "1")],
                )
            }
        };


        let Some(target) = self.routes.resolve(&host, &path, ) else {
            self.metrics.requests_404.fetch_add(1, Relaxed);
            return text(StatusCode::NOT_FOUND, "no route", &[]);
        };
        self.metrics.route_resolutions_total.fetch_add(1, Relaxed);
        self.metrics.note_tier(target.tier);



        self.dispatch_http(req, target, peer).await
    }

    async fn dispatch_http(
        self: &Arc<Self>,
        req: Request<Incoming>,
        target: RouteTarget,
        _peer: SocketAddr,
    ) -> Response<Body> {
        let (parts, body) = req.into_parts();
        let body = match body.collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return text(StatusCode::BAD_REQUEST, "bad body", &[]),
        };
        let hreq = HttpRequest {
            method: parts.method.to_string(),
            url: parts.uri.to_string(),
            path: parts.uri.path().to_string(),
            query: parts
                .uri
                .query()
                .map(|q| parse_query(q))
                .unwrap_or_default(),
            headers: parts
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                .collect(),
            body: body.to_vec(),
        };
        match self.dispatcher.dispatch_http(&target, hreq).await {
            Ok(reply) => {
                let mut b = Response::builder().status(reply.status);
                for (k, v) in &reply.headers {
                    // `x-wisp-*` is the platform's namespace. A guest that
                    // could set `x-wisp-tier` or `x-wisp-version` could lie to
                    // its own operator about which build served a request, so
                    // guest-supplied values in this namespace are dropped
                    // rather than merged.
                    if k.to_ascii_lowercase().starts_with("x-wisp-") {
                        continue;
                    }
                    b = b.header(k, v);
                }
                // Report the selected execution tier to the client,
                // not just our dashboards.
                b = b.header("x-wisp-tier", tier_str(target.tier));
                b = b.header("x-wisp-version", target.version.clone());
                b.body(Full::new(Bytes::from(reply.body))).unwrap()
            }
            Err(e) => {
                self.metrics.requests_failed.fetch_add(1, Relaxed);
                tracing::warn!(error = %e, app = %target.app, "dispatch failed");
                text(StatusCode::BAD_GATEWAY, "dispatch failed", &[])
            }
        }
    }




}

// ---------------------------------------------------------------------------

fn tier_str(t: wisp_abi::host::CodeTier) -> &'static str {
    match t {
        wisp_abi::host::CodeTier::Fast => "fast",
        wisp_abi::host::CodeTier::Optimized => "optimized",
    }
}


fn text(status: StatusCode, body: &str, headers: &[(&str, &str)]) -> Response<Body> {
    let mut b = Response::builder().status(status).header("content-type", "text/plain");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Full::new(Bytes::from(body.to_string()))).unwrap()
}



fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv.to_string(), String::new()),
        })
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;




    #[test]
    fn query_parsing() {
        assert_eq!(
            parse_query("a=1&b=&c"),
            vec![("a".into(), "1".into()), ("b".into(), "".into()), ("c".into(), "".into())]
        );
    }
}
