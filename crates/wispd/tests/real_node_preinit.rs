//! Init-visible config is consumed at preinit, not at runtime: a different
//! guest fixture from the rest of the real-node suite, asserted against the
//! deploy record rather than a dispatch.

#![cfg(feature = "real-backends")]

#[path = "common/preparation.rs"]
mod preparation;
use preparation::LocalDeployment;

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use wispd::config::{AppConfig, CellConfig, NodeConfig, VersionConfig};
use wispd::node::Node;

fn config(dir: &std::path::Path) -> NodeConfig {
    let mut cfg = NodeConfig::dev_default();
    cfg.node = "real-node".into();
    cfg.data_dir = dir.to_path_buf();
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

struct Rig {
    proxy: SocketAddr,
    _admin: SocketAddr,
    _node: Arc<Node>,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn shutdown(self) {
        tokio::task::spawn_blocking(move || drop(self))
            .await
            .unwrap();
    }
}

async fn serve(node: Arc<Node>, dir: tempfile::TempDir) -> Rig {
    let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, admin) = (p.local_addr().unwrap(), a.local_addr().unwrap());
    tokio::spawn(node.proxy.clone().serve(p));
    tokio::spawn(wispd::http::serve(node.clone(), a));
    Rig {
        proxy,
        _admin: admin,
        _node: node,
        _dir: dir,
    }
}

struct Resp {
    status: StatusCode,
    body: String,
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
    let body = r.into_body().collect().await.unwrap().to_bytes();
    Resp {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// Configuration supplied during preparation is observed by the serving guest.
#[tokio::test]
async fn a_real_guests_init_visible_config_is_consumed_at_preinit_not_at_runtime() {
    let path = wisp_fixtures::rust_path("quotes-api");

    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut cfg = config(dir.path());
    let wasm = std::fs::read(path).expect("the guest fixture is built");
    cfg.apps[0].init_visible_vars = [("REGION".to_string(), "nz-akl".to_string())]
        .into_iter()
        .collect();

    let mut manifest: wisp_abi::config::AppManifest =
        toml::from_str("name = 'quotes'").expect("valid manifest");
    manifest.init_visible_vars = cfg.apps[0]
        .init_visible_vars
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let node = Node::build(cfg).expect("a real node must assemble");
    let rec = node
        .real
        .as_ref()
        .expect("real cells")
        .deploy_locally("quotes", "v1", "acme", manifest, &wasm, Default::default())
        .await
        .expect("explicit local preparation");
    assert!(
        rec.from_template,
        "a guest that exports wisp_initialize must yield a template"
    );
    assert!(
        rec.stages.iter().any(|s| s.name == "preinit"),
        "the startup deploy ran the pipeline: {:?}",
        rec.stages
    );
    let pre = rec
        .inner
        .preinit
        .as_ref()
        .expect("a template implies a pre-init summary");
    assert_eq!(
        pre.deferred_reasons
            .iter()
            .map(|(_, reason)| reason.as_str())
            .collect::<Vec<_>>(),
        vec!["clock_now_coarse"],
        "exactly the clock-reading initializer defers"
    );
    assert_eq!(
        pre.init_pal, "sealed",
        "a Rust SDK guest needs no runtime PAL"
    );

    let rig = serve(node, dir).await;

    let r = get(rig.proxy, "quotes.wisp.dev", "/memo").await;
    assert_eq!(r.status, StatusCode::OK, "body: {}", r.body);
    assert!(
        !r.body.contains("capability_violation"),
        "the init-visible imports must have been consumed at deploy time: {}",
        r.body
    );
    assert!(
        r.body.contains("\"config_region\":\"nz-akl\""),
        "the deploy-time config is inside the template: {}",
        r.body
    );

    rig.shutdown().await;
}
