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
use isoplane_abi::host::Dispatcher;
use tokio::net::TcpListener;
use tracing::Instrument;

use crate::metrics::ProxyMetrics;
use crate::response::text;
use crate::routes::RouteResolver;
use crate::shed::{AdmissionControl, Admit, ShedConfig};

pub type Body = UnsyncBoxBody<Bytes, anyhow::Error>;

pub struct Proxy {
    pub routes: Arc<dyn RouteResolver>,
    pub dispatcher: Arc<dyn Dispatcher>,
    pub metrics: Arc<ProxyMetrics>,
    admission: Arc<AdmissionControl>,
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
        shed: ShedConfig,
    ) -> Arc<Self> {
        Arc::new(Proxy {
            admission: Arc::new(AdmissionControl::new(shed, metrics.clone())),
            routes,
            dispatcher,
            metrics,
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
            let (stream, _peer) = match accepted {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    continue;
                }
            };
            self.transports.spawn(self.clone().serve_connection(stream));
        }
        drop(listener);
        self.wait_for_shutdown().await;
        Ok(())
    }

    async fn serve_connection<S>(self: Arc<Self>, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let shutdown = self.shutdown.clone();
        let io = TokioIo::new(stream);
        let svc = service_fn(move |request| self.clone().handle(request));
        let connection = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, svc)
            .with_upgrades();
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
        req: Request<Incoming>,
    ) -> Result<Response<Body>, hyper::Error> {
        let trace = crate::request_trace::RequestTrace::new(&req);
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

        let resp = self.route_and_dispatch(req).instrument(trace.span()).await;
        Ok(trace
            .response(resp)
            .map(|body| crate::response::hold_until_body_end(body.boxed_unsync(), measurement)))
    }

    async fn route_and_dispatch(self: &Arc<Self>, req: Request<Incoming>) -> Response<Body> {
        let host = match req.headers().get(hyper::header::HOST) {
            Some(host) => host.to_str().ok(),
            None => req.uri().host(),
        }
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

        let Some(target) = self.routes.resolve(host, &path) else {
            self.metrics.requests_404.fetch_add(1, Relaxed);
            return text(StatusCode::NOT_FOUND, "no route", &[]);
        };
        self.metrics.note_tier(target.tier);

        let response = self.dispatch_http(req, target).await;
        response.map(|body| crate::response::hold_until_body_end(body, permit))
    }
}
