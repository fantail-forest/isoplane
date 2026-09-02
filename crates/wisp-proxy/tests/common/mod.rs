//! Drives the real proxy over a real TCP socket; the only fake is the cell.

pub mod fake;

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use wisp_abi::host::CodeTier;
use self::fake::FakeCell;
use wisp_proxy::{LocalRouteTable, Proxy, ProxyConfig, ProxyMetrics};

pub struct Rig {
    pub addr: SocketAddr,
    pub proxy: Arc<Proxy>,
    pub cell: Arc<FakeCell>,
    pub routes: Arc<LocalRouteTable>,
}

pub async fn start(cell: Arc<FakeCell>, routes: Arc<LocalRouteTable>, cfg: ProxyConfig) -> Rig {
    let metrics = Arc::new(ProxyMetrics::new());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("route setup");
    let addr = listener.local_addr().expect("route setup");
    let proxy = Proxy::new(routes.clone(), cell.clone(), metrics.clone(), cfg);
    tokio::spawn(proxy.clone().serve(listener));
    Rig {
        addr,
        proxy,
        cell,
        routes,
    }
}

/// Select the application identity and execution tier for an HTTP request.
pub fn quotes_target(version: &str, tier: CodeTier) -> wisp_abi::host::RouteTarget {
    wisp_abi::host::RouteTarget {
        app: "quotes".into(),
        tenant: "acme".into(),
        version: version.into(),
        lifecycle: wisp_abi::config::HandlerLifecycle::Reuse,
        tier,
    }
}

/// One installed application on its expected Host.
pub fn standard_routes() -> Arc<LocalRouteTable> {
    let table = LocalRouteTable::new();
    table.insert(
        quotes_target("v1", CodeTier::Optimized),
        &["quotes.wisp.dev".into()],
    ).expect("route setup");
    Arc::new(table)
}

pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl HttpResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

pub async fn get(
    addr: SocketAddr,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> HttpResponse {
    request(addr, "GET", host, path, headers, Vec::new()).await
}

pub async fn request(
    addr: SocketAddr,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> HttpResponse {
    let stream = TcpStream::connect(addr).await.expect("route setup");
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("route setup");
    tokio::spawn(async move {
        // The connection ends when the response has been read; an error here
        // surfaces as a failed send_request below.
        let _ = conn.await;
    });
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = b.body(Full::new(Bytes::from(body))).expect("route setup");
    let resp = sender.send_request(req).await.expect("route setup");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    HttpResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}
