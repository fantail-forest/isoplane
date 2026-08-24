#![cfg(feature = "real-backends")]

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use wisp_abi::config::AppManifest;
use wisp_abi::dispatch::ResponseHead;
use wispd::config::{AppConfig, CellConfig, NodeConfig, TierConfig, VersionConfig};
use wispd::node::Node;

/// A guest that answers every dispatch with one canned response.
///
/// Hand-written `.wat` rather than the guest SDK, deliberately: if this test
/// fails it must be the wiring, not a guest-SDK regression. The response head
/// is postcard-encoded here and dropped in as a data segment, so the guest's
/// entire job is `resp_new(ptr, len)` and returning `Done { response: h }`.
fn guest(head: &ResponseHead) -> Vec<u8> {
    let encoded = wisp_abi::encode(head).expect("encoding a response head");
    let escaped: String = encoded.iter().map(|b| format!("\\{b:02x}")).collect();
    let len = encoded.len();
    let src = format!(
        r#"
(module
  (import "wisp" "resp_new" (func $resp_new (param i32 i32) (result i32)))
  (memory (export "memory") 2 2)
  (data (i32.const 4096) "{escaped}")
  (global $bump (export "g_bump") (mut i32) (i32.const 16384))

  ;; (size, align) -> ptr. A ring, not a heap: the engine copies each dispatch
  ;; record in and the guest never retains it.
  (func (export "wisp_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const 122880))
      (then (global.set $bump (i32.const 16384))))
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (local.get $p) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  ;; PollStatus::Done {{ response: h }} packs as (h as u64) << 32 (§5.2).
  (func (export "wisp_handle") (param $ptr i32) (param $len i32) (result i64)
    (i64.shl
      (i64.extend_i32_u (call $resp_new (i32.const 4096) (i32.const {len})))
      (i64.const 32)))

  (func (export "wisp_wake") (param $future i64) (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
)
"#
    );
    wat::parse_str(&src).expect("fixture .wat must assemble")
}

fn manifest(name: &str) -> AppManifest {
    toml::from_str(&format!("name = \"{name}\"")).expect("a minimal manifest is valid")
}

fn config(dir: &std::path::Path) -> NodeConfig {
    let mut cfg = NodeConfig::dev_default();
    cfg.node = "real-node".into();
    cfg.data_dir = dir.to_path_buf();
    cfg.cells = vec![CellConfig {
        name: "cell-1".into(),
        max_hot: 4,
    }];
    cfg.apps = vec![AppConfig {
        name: "quotes".into(),
        tenant: "acme".into(),
        limits: Default::default(),
        lifecycle: Default::default(),
        cell: "cell-1".into(),
        hosts: vec!["quotes.wisp.dev".into()],
        path_prefix: "/".into(),
        versions: vec![VersionConfig {
            id: "v1".into(),
            tier: TierConfig::Fast,
        }],
    }];
    cfg
}

struct Rig {
    proxy: SocketAddr,
    admin: SocketAddr,
    node: Arc<Node>,
    _dir: TempDir,
}

struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Rig {
    /// `wisp_host::Cell` owns a `tokio::runtime::Runtime`, and dropping a
    /// runtime from inside an async context panics. In `wispd` the node lives
    /// until the process exits, so this only bites tests — but it bites them
    /// as a confusing panic in a worker thread, so tear down explicitly.
    async fn shutdown(self) {
        tokio::task::spawn_blocking(move || drop(self))
            .await
            .unwrap();
    }
}

async fn start(head: ResponseHead) -> Rig {
    let dir = std::env::temp_dir().join(format!(
        "wispd-real-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let node = Node::build(config(&dir)).expect("a real node must assemble");
    let real = node
        .real
        .clone()
        .expect("the node has its execution backend");
    real.register_version("quotes", "v1", "acme", manifest("quotes"), &guest(&head))
        .expect("registering the module");

    let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, admin) = (p.local_addr().unwrap(), a.local_addr().unwrap());
    tokio::spawn(node.proxy.clone().serve(p));
    tokio::spawn(wispd::http::serve(node.clone(), a));
    Rig {
        proxy,
        admin,
        node,
        _dir: TempDir(dir),
    }
}

struct Resp {
    status: StatusCode,
    body: String,
    headers: hyper::HeaderMap,
}

async fn get(addr: SocketAddr, host: &str, path: &str) -> Resp {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = Request::builder()
        .uri(path)
        .header("host", host)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let r = sender.send_request(req).await.unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn head(status: u16, body: &str) -> ResponseHead {
    ResponseHead {
        status,
        headers: vec![("content-type".into(), "text/plain".into())],
        body: body.as_bytes().to_vec(),
        streaming: false,
    }
}

// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_http_request_runs_real_guest_wasm_end_to_end() {
    let rig = start(head(200, "hello from wasm")).await;

    let r = get(rig.proxy, "quotes.wisp.dev", "/api/quote").await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body, "hello from wasm",
        "the body came out of the guest's linear memory"
    );
    assert_eq!(
        r.headers.get("content-type").unwrap(),
        "text/plain",
        "and so did the headers"
    );
    // Platform headers are still the platform's (§9.3).
    assert_eq!(r.headers.get("x-wisp-version").unwrap(), "v1");
    assert_eq!(r.headers.get("x-wisp-tier").unwrap(), "fast");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_guest_chosen_status_code_reaches_the_client() {
    let rig = start(head(418, "no coffee")).await;
    let r = get(rig.proxy, "quotes.wisp.dev", "/brew").await;
    assert_eq!(r.status, 418);
    assert_eq!(r.body, "no coffee");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_reports_measurements_from_served_requests() {
    // Served requests contribute to both proxy and cell measurements.
    let rig = start(head(200, "ok")).await;
    for i in 0..20 {
        assert_eq!(
            get(rig.proxy, "quotes.wisp.dev", &format!("/{i}"))
                .await
                .status,
            200
        );
    }

    let stream = TcpStream::connect(rig.admin).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = Request::builder()
        .uri("/metrics")
        .header("host", "localhost")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let body = sender
        .send_request(req)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let text = String::from_utf8_lossy(&body).into_owned();

    let metric = |name: &str| -> Option<f64> {
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .find(|l| l.starts_with(name))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    };
    assert_eq!(metric("wisp_proxy_requests_total"), Some(20.0));
    assert_eq!(
        metric("wisp_cell_dispatches_total"),
        Some(20.0),
        "the cell counted the same 20 dispatches the proxy did"
    );
    assert!(!text.contains("NaN"));
    rig.shutdown().await;
}
