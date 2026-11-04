use super::{common, guest, head};
use common::preparation::LocalDeployment;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delimiter_collisions_cannot_substitute_another_applications_module() {
    let directory = common::private_directory().unwrap();
    let mut config = common::config(directory.path());
    let mut second = config.apps[0].clone();
    second.manifest.name = "quotes@team".into();
    second.hosts = vec!["second.example".into()];
    config.apps.push(second);
    let node = isoplaned::Node::build(config).unwrap();
    for (app, version, status) in [("quotes", "team@v1", 201), ("quotes@team", "v1", 202)] {
        node.real
            .deploy_locally(
                app,
                version,
                "acme",
                common::manifest(app),
                &guest(&head(status, app)),
                Default::default(),
            )
            .await
            .unwrap();
    }
    let rig = common::serve(node, directory).await;
    for (app, host, status) in [
        ("quotes", "quotes.isoplane.dev", 201),
        ("quotes@team", "second.example", 202),
    ] {
        let response = common::get(rig.proxy, host, "/").await;
        assert_eq!(
            response.status, status,
            "{app} must retain its exact module identity"
        );
        assert_eq!(response.body, app);
    }
    for path in ["/admin/apps", "/v1/deploy", "/admin/clock", "/admin/splits"] {
        assert_eq!(
            common::get(rig.admin, "localhost", path).await.status,
            404,
            "removed control endpoint {path} must not be exposed"
        );
    }
    rig.shutdown().await;
}
