//! HTTP routing, admission and connection lifecycle.

use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, RETRY_AFTER};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use isoplane_abi::host::{Dispatcher, };
use tokio::net::TcpListener;
use tracing::Instrument;

use crate::metrics::ProxyMetrics;
use crate::response::text;
use crate::routes::RouteResolver;
use crate::shed::{AdmissionControl, Admit, ShedConfig};

pub type Body = UnsyncBoxBody<Bytes, anyhow::Error>;

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Trust host correlation only on the private application-worker listener.
    pub request_origin: crate::request_trace::RequestOrigin,
    pub listen: SocketAddr,
    pub shed: ShedConfig,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            request_origin: crate::request_trace::RequestOrigin::Public,
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
    pub(crate) cfg: ProxyConfig,
    shutdown: tokio_util::sync::CancellationToken,
    pub(crate) transports: tokio_util::task::TaskTracker,
}

struct ResponseMeasurement {
    metrics: Arc<ProxyMetrics>,
    started: Instant,
}

impl Drop for ResponseMeasurement {
    fn drop(&mut self) {
        self.metrics.request_latency.record(self.started.elapsed());
    }
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
            cfg,
            shutdown: tokio_util::sync::CancellationToken::new(),
            transports: tokio_util::task::TaskTracker::new(),
        })
    }




    /// Stops HTTP admission and cancels accepted HTTP transports.
    /// This does not wait for guest activation or other host work to settle.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Joins tracked transport cleanup after the listener's serve future has ended.
    pub async fn wait_for_shutdown(&self) {
        self.transports.close();
        self.transports.wait().await;
    }

    /// Serves until explicitly shut down; cancellation also stops accepted transports.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        let _http_stop = self.shutdown.clone().drop_guard();
        let local = listener.local_addr()?;
        tracing::info!(addr = %local, "proxy listening (plaintext)");
        loop {
            let accepted = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let (stream, peer) = match accepted {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    continue;
                }
            };
            self.transports
                .spawn(self.clone().serve_connection(stream, peer));
        }
        drop(listener);
        self.wait_for_shutdown().await;
        Ok(())
    }

    async fn serve_connection<S>(self: Arc<Self>, stream: S, peer: SocketAddr)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let shutdown = self.shutdown.clone();
        let io = TokioIo::new(stream);
        let svc = service_fn(move |request| self.clone().handle(request, peer));
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, svc);
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {},
            result = connection => if let Err(error) = result {
                tracing::debug!(error = %error, "connection closed");
            },
        }
    }

    pub async fn handle(
        self: Arc<Self>,
        mut req: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Response<Body>, hyper::Error> {
        let trace = crate::request_trace::RequestTrace::worker(&mut req, self.cfg.request_origin);
        if self.shutdown.is_cancelled() {
            return Ok(trace
                .response(text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "proxy is stopping",
                    &[],
                ))
                .map(BodyExt::boxed_unsync));
        }
        let measurement = ResponseMeasurement {
            metrics: self.metrics.clone(),
            started: Instant::now(),
        };
        self.metrics.requests_total.fetch_add(1, Relaxed);
        self.metrics
            .route_generation
            .store(self.routes.generation(), Relaxed);

        let resp = self
            .route_and_dispatch(req, peer)
            .instrument(trace.span())
            .await;
        Ok(trace
            .response(resp)
            .map(|body| crate::response::hold_until_body_end(body.boxed_unsync(), measurement)))
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
        let permit = match self.admission.admit().await {
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



        let response = self.dispatch_http(req, target, peer).await;
        response.map(|body| crate::response::hold_until_body_end(body, permit))
    }
}
