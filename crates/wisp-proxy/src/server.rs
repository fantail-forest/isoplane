//! HTTP routing, admission and connection lifecycle.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, RETRY_AFTER};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use wisp_abi::host::{Dispatcher, };

use crate::dispatch::text;
use crate::metrics::ProxyMetrics;
use crate::routes::RouteResolver;
use crate::shed::{AdmissionControl, Admit, ShedConfig};

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
        tracing::info!(addr = %local, "proxy listening (plaintext)");
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

    pub async fn handle(
        self: Arc<Self>,
        req: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Response<Body>, hyper::Error> {
        let started = Instant::now();
        self.metrics.requests_total.fetch_add(1, Relaxed);
        self.metrics
            .route_generation
            .store(self.routes.generation(), Relaxed);

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

        // Before route resolution: an overloaded node should spend as little
        // as possible on a request it is about to refuse.
        let _permit = match self.admission.admit().await {
            Admit::Granted { _permit } => _permit,
            Admit::Shed => {
                return text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "overloaded",
                    &[(RETRY_AFTER, HeaderValue::from_static("1"))],
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
}
