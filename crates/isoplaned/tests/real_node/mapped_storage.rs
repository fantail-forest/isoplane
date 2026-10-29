//! A node serves every application's mapped template from its own process;
//! failed storage beneath one application fails only that application's requests.

use std::path::{Path, PathBuf};

use isoplane_test_support::{directories_under, mapped_backing};

use super::{config, get, guest_with_functions, serve, Node, ResponseHead};
use crate::common::manifest;
use crate::common::preparation::LocalDeployment;

/// A guest whose initializer runs, so preparation captures a layer mapped from its baseline.
fn initialized(body: &[u8]) -> Vec<u8> {
    guest_with_functions(
        &ResponseHead {
            status: 200,
            headers: vec![],
            body: body.to_vec(),
            streaming: false,
        },
        r#"(func (export "isoplane_initialize") (result i64)
            (i32.store (i32.const 64) (i32.const 123)) i64.const 0)"#,
        "",
    )
}

/// The published baseline of `deployed`, found under the node's data directory.
fn baseline(data: &Path, deployed: &isoplaned::real::Deployed) -> PathBuf {
    let key = serde_json::to_value(
        &deployed
            .prepared
            .as_ref()
            .expect("a prepared deployment")
            .artifact_key,
    )
    .expect("encode the artifact key");
    let key = key.as_str().expect("an artifact key is a string");
    directories_under(data)
        .into_iter()
        .find(|path| path.file_name().is_some_and(|name| name == key))
        .map(|path| path.join("prepared").join("baseline.bin"))
        .expect("the deployment's artifact directory must exist under the node's data")
}

/// Deploys `name` as `acme`'s prepared application answering its own name.
async fn deploy(real: &isoplaned::real::RealCells, name: &str) -> isoplaned::real::Deployed {
    real.deploy_locally(
        name,
        "v1",
        "acme",
        manifest(name),
        &initialized(name.as_bytes()),
        Default::default(),
    )
    .await
    .expect("deploy a prepared application")
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_storage_beneath_one_application_fails_only_its_requests() {
    let directory = tempfile::tempdir().expect("owned node files");
    let mut cfg = config(directory.path());
    let mut ledger = cfg.apps[0].clone();
    ledger.name = "ledger".into();
    ledger.hosts = vec!["ledger.isoplane.dev".into()];
    cfg.apps.push(ledger);
    let node = Node::build(cfg).expect("real node");
    let real = node.real.clone().expect("real cells");
    let failed = baseline(directory.path(), &deploy(&real, "quotes").await);
    deploy(&real, "ledger").await;
    drop(real);
    let rig = serve(node, directory).await;
    assert_eq!(
        get(rig.proxy, "quotes.isoplane.dev", "/").await.body,
        "quotes",
        "the application must serve while its storage is intact"
    );
    mapped_backing::truncate(&failed);
    let response = get(rig.proxy, "quotes.isoplane.dev", "/").await;
    assert!(
        response.status.is_server_error() && response.body.starts_with("storage_fault"),
        "a request whose template storage failed must fail as a storage fault: {} {}",
        response.status,
        response.body
    );
    assert_eq!(
        get(rig.proxy, "ledger.isoplane.dev", "/").await.body,
        "ledger",
        "another application in the same process must keep serving"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_serves_again_once_its_failed_storage_reads_back() {
    let directory = tempfile::tempdir().expect("owned node files");
    let node = Node::build(config(directory.path())).expect("real node");
    let real = node.real.clone().expect("real cells");
    let failed = baseline(directory.path(), &deploy(&real, "quotes").await);
    drop(real);
    let original = std::fs::read(&failed).expect("read the published baseline");
    let rig = serve(node, directory).await;
    assert_eq!(
        get(rig.proxy, "quotes.isoplane.dev", "/").await.body,
        "quotes",
        "the application must serve while its storage is intact"
    );
    mapped_backing::truncate(&failed);
    assert!(
        get(rig.proxy, "quotes.isoplane.dev", "/")
            .await
            .body
            .starts_with("storage_fault"),
        "the read of failed storage must fail its request"
    );
    mapped_backing::restore(&failed, &original);
    assert_eq!(
        get(rig.proxy, "quotes.isoplane.dev", "/").await.body,
        "quotes",
        "once its artifact verifies again, the application must serve without a node restart"
    );
    rig.shutdown().await;
}
