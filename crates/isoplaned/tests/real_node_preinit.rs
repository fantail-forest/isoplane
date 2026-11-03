//! Init-visible config is consumed at preinit, not at runtime: a different
//! guest fixture from the rest of the real-node suite, asserted against the
//! deploy record rather than a dispatch.

#[path = "common/runtime_inventory.rs"]
mod runtime_inventory;

#[path = "common/private_files.rs"]
mod private_files;

#[path = "common/preparation.rs"]
mod preparation;
use preparation::LocalDeployment;

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;

use isoplaned::config::{AppConfig, NodeConfig};
use isoplaned::node::Node;
use tokio::net::TcpStream;

fn config(dir: &std::path::Path) -> NodeConfig {
    let mut cfg = NodeConfig::new("acme");
    cfg.node = "real-node".into();
    cfg.data_dir = dir.to_path_buf();
    cfg.engine = isoplaned::config::EngineLimits {
        max_instances: 16,
        memory_limit_mib: 128,
        guard_mib: 64,
        ..Default::default()
    };
    cfg.preparation = Some(toml::from_str("").unwrap());
    cfg.preparation.as_mut().unwrap().runtimes = Some(runtime_inventory::install(dir));
    cfg.apps = vec![AppConfig {
        manifest: toml::from_str("name = 'quotes'\nguest_memory_bytes = 134217728").unwrap(),
        hosts: vec!["quotes.isoplane.dev".into()],
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
    node.routes
        .insert(
            isoplane_abi::host::RouteTarget {
                app: "quotes".into(),
                tenant: "acme".into(),
                version: "v1".into(),
                tier: isoplane_abi::host::CodeTier::Fast,
            },
            &["quotes.isoplane.dev".into()],
        )
        .unwrap();
    let p = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (proxy, admin) = (p.local_addr().unwrap(), a.local_addr().unwrap());
    tokio::spawn(node.proxy.clone().serve(p));
    tokio::spawn(isoplaned::http::serve(node.clone(), a));
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
    let path = isoplane_fixtures::rust_path("quotes-api");

    let dir = private_files::directory().expect("a temporary directory");
    let cfg = config(dir.path());
    let wasm = std::fs::read(path).expect("the guest fixture is built");
    let mut manifest: isoplane_abi::config::AppManifest =
        toml::from_str("name = 'quotes'").expect("valid manifest");
    manifest.init_visible_vars = vec![("REGION".to_string(), "nz-akl".to_string())];
    let node = Node::build(cfg).expect("a real node must assemble");
    let rec = node
        .real
        .as_ref()
        .deploy_locally("quotes", "v1", "acme", manifest, &wasm, Default::default())
        .await
        .expect("explicit local preparation");
    assert!(
        rec.stages.iter().any(|s| s.name == "preinit"),
        "the startup deploy ran the pipeline: {:?}",
        rec.stages
    );
    let pre = &rec.initialized;
    assert_eq!(
        pre.deferred_reasons
            .iter()
            .map(|(_, reason)| reason.as_str())
            .collect::<Vec<_>>(),
        vec!["clock_now_coarse"],
        "exactly the clock-reading initializer defers"
    );

    let rig = serve(node, dir).await;

    let r = get(rig.proxy, "quotes.isoplane.dev", "/memo").await;
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
