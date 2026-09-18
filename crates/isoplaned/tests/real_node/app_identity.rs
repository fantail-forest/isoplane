use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;

use super::{common, guest, head};
use common::preparation::LocalDeployment;

async fn two_apps(first_version: &str, second_version: &str) -> common::Rig {
    let directory = tempfile::tempdir().expect("a unique node directory");
    let mut config = common::config(directory.path());
    config.apps[0].versions[0].id = first_version.into();
    let mut second = config.apps[0].clone();
    second.name = "quotes@team".into();
    second.tenant = "another-tenant".into();
    second.hosts = vec!["second.example".into()];
    second.versions[0].id = second_version.into();
    config.apps.push(second);
    let node = isoplaned::node::Node::build(config)
        .expect("distinct app identities must form a valid node");
    let rig = common::serve(node, directory).await;
    for (app, tenant, version, status) in [
        ("quotes", "acme", first_version, 201),
        ("quotes@team", "another-tenant", second_version, 202),
    ] {
        rig.node
            .real
            .as_ref()
            .expect("real cells")
            .deploy_locally(
                app,
                version,
                tenant,
                common::manifest(app),
                &guest(&head(status, app)),
                Default::default(),
            )
            .await
            .expect("explicit test preparation");
    }
    rig
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delimiter_collisions_cannot_hide_another_apps_registered_version() {
    let rig = two_apps("team@v1", "v1").await;
    for (app, tenant, version, host, status) in [
        ("quotes", "acme", "team@v1", "quotes.isoplane.dev", 201),
        ("quotes@team", "another-tenant", "v1", "second.example", 202),
    ] {
        let response = common::get(rig.proxy, host, "/").await;
        assert_eq!(
            response.status, status,
            "each app must continue executing its own module"
        );
        assert_eq!(
            response.body, app,
            "a compound identity cannot substitute another app's guest"
        );
    }
    rig.shutdown().await;
}
