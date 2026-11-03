use super::common;
use isoplaned::{Node, NodeConfig};
use std::net::SocketAddr;
use std::sync::Arc;

struct Serving {
    node: Arc<Node>,
    address: SocketAddr,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Serving {
    async fn start(config: NodeConfig) -> Self {
        let node = tokio::task::spawn_blocking(move || Node::build(config))
            .await
            .unwrap()
            .unwrap();
        node.restore_installed()
            .await
            .expect("restore every selected application before listening");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(node.clone().serve(listener));
        Self {
            node,
            address,
            task,
        }
    }
    async fn stop(self) {
        self.node.shutdown();
        self.task.await.unwrap().unwrap();
        self.node.wait_for_shutdown().await.unwrap();
        tokio::task::spawn_blocking(move || drop(self.node))
            .await
            .unwrap();
    }
}

fn config(directory: &std::path::Path) -> NodeConfig {
    let mut config = common::config(directory);
    let version = isoplane_fixtures::release_version();
    let runtimes = directory.join("runtimes");
    let release = runtimes.join(&version);
    std::fs::create_dir_all(&release).unwrap();
    let module = isoplane_fixtures::mono_runtime_module();
    std::fs::hard_link(&module, release.join("Isoplane.MonoRuntime.wasm")).unwrap();
    std::fs::copy(
        module.parent().unwrap().join("runtime.json"),
        release.join("runtime.json"),
    )
    .unwrap();
    std::fs::write(
        runtimes.join("inventory.json"),
        serde_json::to_vec(&serde_json::json!({"sdkVersions":[version]})).unwrap(),
    )
    .unwrap();
    config.preparation = Some(isoplaned::preparation::PreparationConfig {
        runtimes: Some(runtimes),
        initialization_occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
        initialization_wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
        initialization_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
        initialization_log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
    });
    let mut ledger = config.apps[0].clone();
    ledger.manifest.name = "ledger".into();
    ledger.hosts = vec!["ledger.example".into()];
    config.apps.push(ledger);
    config
}

fn selection_bytes(config: &NodeConfig, app: &str) -> Vec<u8> {
    std::fs::read(
        config
            .data_dir
            .join("selected")
            .join(format!("{}.json", blake3::hash(app.as_bytes()).to_hex())),
    )
    .unwrap_or_else(|error| panic!("reading selected application `{app}`: {error}"))
}
fn artifact(config: &NodeConfig, app: &str) -> std::path::PathBuf {
    let selected: serde_json::Value =
        serde_json::from_slice(&selection_bytes(config, app)).unwrap();
    config
        .data_dir
        .join("registry")
        .join(selected["receipt"]["artifactKey"].as_str().unwrap())
}

fn occupied_listeners_preserve_removed_applications(config: &NodeConfig, app: &str) {
    let selected = selection_bytes(config, app);
    let backing = artifact(config, app);
    for occupy_proxy in [true, false] {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut failed = config.clone();
        failed
            .apps
            .retain(|configured| configured.manifest.name != app);
        failed.proxy.listen = "127.0.0.1:0".parse().unwrap();
        failed.admin.listen = "127.0.0.1:0".parse().unwrap();
        if occupy_proxy {
            failed.proxy.listen = occupied.local_addr().unwrap();
        } else {
            failed.admin.listen = occupied.local_addr().unwrap();
        }
        let path = config.data_dir.join("occupied-listener.toml");
        std::fs::write(&path, toml::to_string(&failed).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
            .args(["serve", "--config"])
            .arg(path)
            .env_remove("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")
            .env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "startup must fail while its listener is occupied"
        );
        assert_eq!(
            selection_bytes(config, app),
            selected,
            "failed startup must preserve the removed application's selection"
        );
        assert!(
            backing.is_dir(),
            "failed startup must preserve the removed application's artifact"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_portable_apps_redeploy_on_restart_and_failed_preparation_preserves_live_state() {
    let directory = common::private_directory().unwrap();
    let mut config = config(directory.path());
    let source = directory.path().join("quotes.json");
    isoplane_control::private_files::write_new(&source, br#"{"key":"captured-first"}"#).unwrap();
    config
        .application_settings
        .push(isoplaned::settings::ApplicationSettings {
            application: "quotes".into(),
            configuration_file: Some(source.clone()),
            allowed_destinations: Vec::new(),
        });
    let cancellation = tokio_util::sync::CancellationToken::new();
    for (app, package) in [("quotes", "alpha"), ("ledger", "beta")] {
        let result = isoplaned::node::deploy::deploy(
            config.clone(),
            app,
            &isoplane_fixtures::mono_package(package),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(result.restart_required);
    }
    let original = artifact(&config, "quotes");
    let ledger = selection_bytes(&config, "ledger");
    occupied_listeners_preserve_removed_applications(&config, "ledger");
    let mut incompatible = config.clone();
    incompatible.apps.pop();
    incompatible.apps[0]
        .manifest
        .init_visible_vars
        .push(("changed".into(), "policy".into()));
    let failed = tokio::task::spawn_blocking(move || Node::build(incompatible))
        .await
        .unwrap()
        .unwrap();
    assert!(failed.restore_installed().await.is_err());
    failed.shutdown();
    failed.wait_for_shutdown().await.unwrap();
    tokio::task::spawn_blocking(move || drop(failed))
        .await
        .unwrap();
    assert_eq!(
        selection_bytes(&config, "ledger"),
        ledger,
        "failed startup cannot retire a sibling removed from that failed configuration"
    );
    std::fs::remove_file(&source).unwrap();
    let running = Serving::start(config.clone()).await;
    assert_eq!(
        common::get(running.address, "quotes.isoplane.dev", "/one")
            .await
            .body,
        "alpha:/one"
    );
    let beta = common::get(running.address, "ledger.example", "/two").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&beta.body).unwrap()["Path"],
        "/two"
    );
    assert_eq!(
        common::get(running.address, "unknown.example", "/")
            .await
            .status,
        404
    );

    let before = selection_bytes(&config, "quotes");
    let invalid = directory.path().join("invalid.tar");
    std::fs::write(&invalid, b"invalid bundle").unwrap();
    assert!(
        isoplaned::node::deploy::deploy(config.clone(), "quotes", &invalid, &cancellation)
            .await
            .is_err()
    );
    assert_eq!(selection_bytes(&config, "quotes"), before);
    assert_eq!(
        common::get(running.address, "quotes.isoplane.dev", "/still")
            .await
            .body,
        "alpha:/still"
    );

    assert!(
        isoplaned::node::deploy::deploy(
            config.clone(),
            "quotes",
            &isoplane_fixtures::mono_package("beta"),
            &cancellation
        )
        .await
        .is_err(),
        "new preparation requires its selected private source"
    );
    assert_eq!(selection_bytes(&config, "quotes"), before);
    assert_eq!(
        common::get(running.address, "quotes.isoplane.dev", "/settings-missing")
            .await
            .body,
        "alpha:/settings-missing"
    );
    isoplane_control::private_files::write_new(&source, br#"{"key":"captured-second"}"#).unwrap();
    let mut oversized = config.clone();
    oversized.apps[0]
        .manifest
        .init_visible_vars
        .push(("large".into(), "x".repeat(1024 * 1024)));
    let error = isoplaned::node::deploy::deploy(
        oversized,
        "quotes",
        &isoplane_fixtures::mono_package("beta"),
        &cancellation,
    )
    .await
    .err()
    .expect("an unreadable selection must not replace the current deployment");
    assert!(
        matches!(
            error.downcast_ref::<isoplaned::installed::InstallError>(),
            Some(isoplaned::installed::InstallError::TooLarge(_))
        ),
        "{error:#}"
    );
    assert_eq!(selection_bytes(&config, "quotes"), before);
    assert_eq!(selection_bytes(&config, "ledger"), ledger);
    assert_eq!(
        common::get(running.address, "quotes.isoplane.dev", "/still-current")
            .await
            .body,
        "alpha:/still-current"
    );
    isoplaned::node::deploy::deploy(
        config.clone(),
        "quotes",
        &isoplane_fixtures::mono_package("beta"),
        &cancellation,
    )
    .await
    .unwrap();
    assert!(
        original.is_dir(),
        "old active recovery backing must survive offline selection"
    );
    assert_eq!(selection_bytes(&config, "ledger"), ledger);
    assert_eq!(
        common::get(running.address, "quotes.isoplane.dev", "/old")
            .await
            .body,
        "alpha:/old"
    );
    std::fs::remove_file(&source).unwrap();
    running.stop().await;

    let restarted = Serving::start(config.clone()).await;
    let updated = common::get(restarted.address, "quotes.isoplane.dev", "/new").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&updated.body).unwrap()["Path"],
        "/new"
    );
    assert_eq!(selection_bytes(&config, "ledger"), ledger);
    assert!(
        !original.exists(),
        "unselected old backing is reclaimed after its execution owners end"
    );
    let sibling = common::get(restarted.address, "ledger.example", "/unchanged").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&sibling.body).unwrap()["Path"],
        "/unchanged"
    );
    restarted.stop().await;
}
