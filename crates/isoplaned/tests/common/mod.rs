//! A real node under test: real engine, cells, guest wasm and proxy, on
//! ephemeral ports, with one app `quotes@v1` deployed through the pipeline.

pub mod preparation;
use preparation::LocalDeployment;

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use isoplane_abi::config::{AppManifest, InitSandbox};
use isoplane_host::egress::DestinationPolicy;
use isoplaned::config::{AppConfig, CellConfig, NodeConfig, VersionConfig};
use isoplaned::node::Node;
use tokio::net::TcpStream;

pub fn manifest(name: &str) -> AppManifest {
    toml::from_str(&format!("name = \"{name}\"")).expect("a minimal manifest is valid")
}

pub fn config(dir: &std::path::Path) -> NodeConfig {
    let mut cfg = NodeConfig::dev_default();
    cfg.node = "real-node".into();
    cfg.data_dir = dir.to_path_buf();
    cfg.engine = NodeConfig::from_toml(include_str!("../fixtures/node.toml"))
        .expect("the test node configuration parses")
        .engine;
    seed_compiled_runtime(dir);
    cfg.cells = vec![CellConfig {
        name: "cell-1".into(),
    }];
    cfg.apps = vec![AppConfig {
        name: "quotes".into(),
        tenant: "acme".into(),
        limits: Default::default(),
        lifecycle: Default::default(),
        cell: "cell-1".into(),
        hosts: vec!["quotes.isoplane.dev".into()],
        path_prefix: "/".into(),
        versions: vec![VersionConfig { id: "v1".into() }],
    }];
    cfg
}

/// Gives the node in `dir` the shared runtime Task compiled for the test engine, so that
/// deploying a managed guest prepares it without compiling the runtime again. The files are
/// hard links: the compilation cache replaces entries by renaming, never in place.
fn seed_compiled_runtime(dir: &std::path::Path) {
    let artifacts = isoplaned::real::RealPaths::under(dir).artifacts;
    std::fs::create_dir_all(&artifacts).expect("create the node's artifact store");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&artifacts, std::fs::Permissions::from_mode(0o700))
            .expect("make the artifact store private to its node");
    }
    let seed = isoplane_fixtures::node_runtime_artifacts();
    for entry in std::fs::read_dir(&seed).expect("list the prepared node runtime") {
        let entry = entry.expect("read the prepared node runtime");
        let target = artifacts.join(entry.file_name());
        // A node restarted on the same directory already holds the entry. Copying onto it
        // would truncate the shared file before reading it.
        if target.exists() {
            continue;
        }
        std::fs::hard_link(entry.path(), &target)
            .or_else(|_| std::fs::copy(entry.path(), &target).map(drop))
            .unwrap_or_else(|e| panic!("seed {}: {e}", target.display()));
    }
}

pub struct Rig {
    pub proxy: SocketAddr,
    pub admin: SocketAddr,
    pub node: Arc<Node>,
    _dir: tempfile::TempDir,
    listeners: Vec<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

impl Rig {
    /// Join cell workers without blocking the test's async executor.
    pub async fn shutdown(mut self) {
        self.node.shutdown();
        for listener in self.listeners.drain(..) {
            listener.abort();
            match listener.await {
                Ok(result) => result.expect("the test listener must not fail"),
                Err(error) => assert!(
                    error.is_cancelled(),
                    "the test listener must stop without panicking: {error}"
                ),
            }
        }
        self.node
            .wait_for_shutdown()
            .await
            .expect("application processes must be reaped");
        tokio::task::spawn_blocking(move || drop(self))
            .await
            .unwrap();
    }
}

/// A node serving `wasm` as `quotes@v1`, deployed through the real pipeline
/// under `sandbox`.
pub async fn start_with(wasm: impl Into<preparation::Guest>, sandbox: InitSandbox) -> Rig {
    start_with_policy(wasm, sandbox, None, |_| {}).await
}

pub async fn start_with_policy(
    wasm: impl Into<preparation::Guest>,
    sandbox: InitSandbox,
    destinations: Option<Arc<dyn DestinationPolicy>>,
    configure: impl FnOnce(&mut NodeConfig),
) -> Rig {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut cfg = config(dir.path());
    configure(&mut cfg);
    let node = match destinations {
        Some(policy) => Node::build_with_destinations(cfg, policy),
        None => Node::build(cfg),
    }
    .expect("a real node must assemble");
    let real = node
        .real
        .clone()
        .expect("the node has its execution backend");
    real.deploy_locally("quotes", "v1", "acme", manifest("quotes"), wasm, sandbox)
        .await
        .expect("deploying the module");
    serve(node, dir).await
}

/// `node` on ephemeral proxy and admin ports.
pub async fn serve(node: Arc<Node>, dir: tempfile::TempDir) -> Rig {
    let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, admin) = (p.local_addr().unwrap(), a.local_addr().unwrap());
    let listeners = vec![
        tokio::spawn(node.clone().serve(p)),
        tokio::spawn(isoplaned::http::serve(node.clone(), a)),
    ];
    Rig {
        proxy,
        admin,
        node,
        _dir: dir,
        listeners,
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub body: String,
    pub headers: hyper::HeaderMap,
}

pub async fn get(addr: SocketAddr, host: &str, path: &str) -> Resp {
    let req = Request::builder()
        .uri(path)
        .header("host", host)
        .body(Full::new(Bytes::new()))
        .unwrap();
    request(addr, req).await
}

pub async fn request(addr: SocketAddr, req: Request<Full<Bytes>>) -> Resp {
    let mut connection = HttpConnection::connect(addr).await;
    let r = connection.request(req).await;
    let status = r.status();
    let headers = r.headers().clone();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// A test client whose connection task is cancelled when its owner is dropped.
pub struct HttpConnection<B = Full<Bytes>> {
    sender: hyper::client::conn::http1::SendRequest<B>,
    driver: tokio::task::JoinHandle<Result<(), hyper::Error>>,
}
impl<B> HttpConnection<B>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub async fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address)
            .await
            .expect("test HTTP connection");
        let (sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .expect("test HTTP handshake");
        Self {
            sender,
            driver: tokio::spawn(driver),
        }
    }
    pub async fn request(&mut self, request: Request<B>) -> hyper::Response<hyper::body::Incoming> {
        self.sender
            .send_request(request)
            .await
            .expect("the test response must arrive")
    }
}
impl<B> Drop for HttpConnection<B> {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
