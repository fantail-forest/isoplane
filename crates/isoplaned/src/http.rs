//! Operator health and metrics listener.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::node::Node;

pub type AdminBody = BoxBody<Bytes, Infallible>;

fn json(status: StatusCode, v: serde_json::Value) -> Response<AdminBody> {
    let body = serde_json::to_vec_pretty(&v).expect("a serde_json::Value always serialises");
    let mut resp = Response::new(Full::new(Bytes::from(body)).boxed());
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    resp
}

fn plain(status: StatusCode, body: String) -> Response<AdminBody> {
    let mut resp = Response::new(Full::new(Bytes::from(body)).boxed());
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    resp
}

/// Serves health and metrics until cancelled by its owner.
pub async fn serve(node: Arc<Node>, listener: TcpListener) -> anyhow::Result<()> {
    serve_until(
        node,
        listener,
        tokio_util::sync::CancellationToken::new(),
        tokio_util::task::TaskTracker::new(),
    )
    .await
}

/// Cancels and joins accepted operator connections when shutdown is requested.
/// The owner can retain `connections` to await cleanup if this future is cancelled.
pub async fn serve_until(
    node: Arc<Node>,
    listener: TcpListener,
    shutdown: tokio_util::sync::CancellationToken,
    connections: tokio_util::task::TaskTracker,
) -> anyhow::Result<()> {
    let _stop = shutdown.clone().drop_guard();
    let local = listener.local_addr()?;
    tracing::info!(addr = %local, "admin/metrics listening");
    let result = loop {
        let accepted = tokio::select! {
            biased;
            () = shutdown.cancelled() => break Ok(()),
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => break Err(error.into()),
        };
        connections.spawn(serve_connection(node.clone(), stream, shutdown.clone()));
    };
    drop(listener);
    shutdown.cancel();
    connections.close();
    connections.wait().await;
    result
}

async fn serve_connection<S>(
    node: Arc<Node>,
    stream: S,
    shutdown: tokio_util::sync::CancellationToken,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let svc = service_fn(move |request| {
        let node = node.clone();
        async move { Ok::<_, Infallible>(handle(node, request).await) }
    });
    let connection = hyper::server::conn::http1::Builder::new().serve_connection(io, svc);
    tokio::select! {
        biased;
        () = shutdown.cancelled() => {},
        result = connection => if let Err(error) = result {
            tracing::debug!(error = %error, "admin connection closed");
        },
    }
}

async fn handle(node: Arc<Node>, request: Request<Incoming>) -> Response<AdminBody> {
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/healthz") => json(StatusCode::OK, serde_json::json!({"status":"ok"})),
        (&Method::GET, "/metrics") => plain(
            StatusCode::OK,
            crate::metrics::render(&node.cfg.node, &node.metrics, node.admin.snapshot()),
        ),
        _ => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error":"not found"}),
        ),
    }
}
