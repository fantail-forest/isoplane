//! A real node under test: real engine, cells, guest wasm and proxy, on
//! ephemeral ports, with one app `quotes@v1` deployed through the pipeline.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use wisp_abi::config::{AppManifest, InitSandbox};
use wispd::config::{AppConfig, CellConfig, NodeConfig, VersionConfig};
use wispd::node::Node;

pub fn manifest(name: &str) -> AppManifest {
    toml::from_str(&format!("name = \"{name}\"")).expect("a minimal manifest is valid")
}

pub fn config(dir: &std::path::Path) -> NodeConfig {
    let mut cfg = NodeConfig::dev_default();
    cfg.node = "real-node".into();
    cfg.data_dir = dir.to_path_buf();
    // Several nodes are built concurrently in one process; the default
    // address-space reservation fits only a handful. The memory ceiling is
    // the default application's, which a .NET guest's minimum memory needs.
    cfg.engine = wispd::config::EngineLimits {
        max_instances: 16,
        max_memory_mib: 128,
        guard_mib: 64,
    };
    cfg.cells = vec![CellConfig {
        name: "cell-1".into(),
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
            wasm: None,
        }],
        init_visible_vars: Default::default(),
        init_sandbox: Default::default(),
    }];
    cfg
}

pub struct Rig {
    pub proxy: SocketAddr,
    pub admin: SocketAddr,
    pub node: Arc<Node>,
    dir: tempfile::TempDir,
}

impl Rig {
    pub fn data_dir(&self) -> &std::path::Path {
        self.dir.path()
    }

    /// Join cell workers without blocking the test's async executor.
    pub async fn shutdown(self) {
        tokio::task::spawn_blocking(move || drop(self))
            .await
            .unwrap();
    }
}

/// A node serving `wasm` as `quotes@v1`, deployed through the real pipeline
/// under `sandbox`.
pub async fn start_with(wasm: Vec<u8>, sandbox: InitSandbox) -> Rig {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let node = Node::build(config(dir.path())).expect("a real node must assemble");
    let real = node
        .real
        .clone()
        .expect("the node has its execution backend");
    real.deploy_version("quotes", "v1", "acme", manifest("quotes"), &wasm, sandbox)
        .await
        .expect("deploying the module");
    serve(node, dir).await
}

/// `node` on ephemeral proxy and admin ports.
pub async fn serve(node: Arc<Node>, dir: tempfile::TempDir) -> Rig {
    let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, admin) = (p.local_addr().unwrap(), a.local_addr().unwrap());
    tokio::spawn(node.proxy.clone().serve(p));
    tokio::spawn(wispd::http::serve(node.clone(), a));
    Rig {
        proxy,
        admin,
        node,
        dir,
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub body: String,
    pub headers: hyper::HeaderMap,
}

pub async fn get(addr: SocketAddr, host: &str, path: &str) -> Resp {
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
