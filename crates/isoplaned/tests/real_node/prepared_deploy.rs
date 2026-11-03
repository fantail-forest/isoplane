struct Refusal;
#[async_trait::async_trait]
impl isoplane_prepare::Preparation for Refusal {
    async fn prepare(
        &self,
        _wasm: &[u8],
        _request: &isoplane_prepare::BuildRequest,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<isoplane_prepare::BuildProduct, isoplane_prepare::PrepareError> {
        Err(isoplane_prepare::PrepareError::Cancelled)
    }

    #[cfg(target_os = "linux")]
    async fn prepare_runtime_baseline(
        &self,
        _wasm: &[u8],
        _request: &isoplane_prepare::BaselineRequest,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<isoplane_prepare::BuildProduct, isoplane_prepare::PrepareError> {
        Err(isoplane_prepare::PrepareError::Cancelled)
    }

    #[cfg(target_os = "linux")]
    async fn prepare_over(
        &self,
        _baseline: &isoplane_prepare::PublishedBaseline,
        _request: &isoplane_prepare::BuildRequest,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<isoplane_prepare::BuildProduct, isoplane_prepare::PrepareError> {
        Err(isoplane_prepare::PrepareError::Cancelled)
    }
}

use super::{get, guest_with_functions, manifest, serve, Node, ResponseHead};
use crate::common::bare_config as config;
use isoplane_control::DeployOptions;
use isoplane_control::VersionId;
use isoplane_prepare::RuntimeProfile;
use isoplaned::deploy::PreparationContext;

#[tokio::test(flavor = "multi_thread")]
async fn prepared_deployment_serves_initialized_state_without_repeating_initialization() {
    let directory = crate::common::private_directory().expect("owned node files");
    use isoplaned::settings::{LocalFiles, RuntimeSettingsSource, SettingsOwner};
    let settings_file = tempfile::NamedTempFile::new_in(directory.path()).expect("private source");
    let first = br#"{"key":"private-first"}"#;
    std::fs::write(settings_file.path(), first).expect("initial source");
    let mut cfg = config(directory.path());
    cfg.application_settings
        .push(isoplaned::settings::ApplicationSettings {
            application: "quotes".into(),
            configuration_file: Some(settings_file.path().to_owned()),
            allowed_destinations: Vec::new(),
        });
    let source = LocalFiles::configured(cfg.owner.clone(), cfg.application_settings.clone())
        .expect("operator source");
    let snapshot = source
        .settings(SettingsOwner {
            tenant: "acme",
            application: "quotes",
        })
        .expect("preparation selection");
    let node = Node::build(cfg).expect("real node");
    let real = node.real.clone();
    let wasm = guest_with_functions(
        &ResponseHead {
            status: 200,
            headers: vec![],
            body: b"prepared deployment".to_vec(),
            streaming: false,
        },
        &format!(
            r#"(import "isoplane" "runtime_config_get" (func $settings (param i32 i32) (result i32)))
        (global $initializations (export "initializations") (mut i32) (i32.const 0))
        (func (export "isoplane_initialize") (result i64)
            (global.set $initializations (i32.add (global.get $initializations) (i32.const 1)))
            (if (i32.ne (call $settings (i32.const 8192) (i32.const 1024)) (i32.const {})) (then unreachable))
            (i32.store (i32.const 64) (i32.const 123)) i64.const 0)"#,
            first.len()
        ),
        &format!(
            r#"(if (i32.ne (global.get $initializations) (i32.const 1)) (then unreachable))
        (if (i32.ne (i32.load (i32.const 64)) (i32.const 123)) (then unreachable))
        (if (i32.ne (call $settings (i32.const 8192) (i32.const 1024)) (i32.const {})) (then unreachable))"#,
            first.len()
        ),
    );
    let provider = real.local_preparation().expect("portable preparation");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let context = PreparationContext {
        network: crate::common::preparation::network("acme", &manifest("quotes")),
        runtime_configuration: snapshot.configuration,
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: real.engine(),
        provider: &provider,
        runtime: RuntimeProfile::raw_wasm(),
        occupancy_ms: 1000,
        wall_ms: 5000,
        max_passes: 4,
        log_bytes: 1024,
        cancellation: &cancellation,
    };
    std::fs::write(settings_file.path(), br#"{"key":"private-second"}"#)
        .expect("source changes before registration");
    let deployed = real
        .deployer
        .prepare_application(
            &manifest("quotes"),
            &wasm,
            &DeployOptions {
                version: VersionId("v1".into()),
                tenant: "acme".into(),
                init_sandbox: Default::default(),
            },
            &context,
        )
        .await
        .expect("prepared deployment through actual cell registration");
    assert_eq!(deployed.initialized.passes, 1, "producer replay count");
    assert!(deployed
        .stages
        .iter()
        .any(|stage| stage.name == "prepared_encoding"));
    let (installed, _, _) = deployed.into_application();
    let receipt = installed.receipt.clone();
    real.install_application("acme", installed)
        .await
        .expect("actual registration");
    let artifact: String = receipt.artifact_key.clone().into();
    let prepared = node
        .cfg
        .data_dir
        .join("registry")
        .join(artifact)
        .join("prepared");
    let digest = hex::decode(&receipt.manifest_sha256)
        .unwrap()
        .try_into()
        .unwrap();
    real.engine()
        .check_prepared_receipt(&prepared, digest, receipt.runtime_profile.digest().unwrap())
        .expect("selected receipt matches the published local template");
    let duplicate = real
        .deployer
        .prepare_application(
            &manifest("quotes"),
            &wasm,
            &DeployOptions {
                version: VersionId("v1".into()),
                tenant: "acme".into(),
                init_sandbox: Default::default(),
            },
            &context,
        )
        .await
        .expect("duplicate candidate");
    assert!(
        real.install_application("acme", duplicate.into_application().0)
            .await
            .is_err(),
        "existing captured state refuses replacement under the same deployment identity"
    );
    let rig = serve(node, directory).await;
    let response = get(rig.proxy, "quotes.isoplane.dev", "/").await;
    assert_eq!(
        response.status,
        hyper::StatusCode::OK,
        "a once-initialized guest must serve through the real proxy"
    );
    assert_eq!(
        response.body, "prepared deployment",
        "the installed artifact must retain the guest response"
    );
    drop(real);
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preparation_refusal_reaches_the_caller_without_publishing_artifacts() {
    let directory = crate::common::private_directory().expect("owned node files");
    let node = Node::build(config(directory.path())).expect("real node");
    let real = node.real.clone();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let context = PreparationContext {
        network: crate::common::preparation::network("acme", &manifest("quotes")),
        runtime_configuration: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: real.engine(),
        provider: &Refusal,
        runtime: RuntimeProfile::raw_wasm(),
        occupancy_ms: 1000,
        wall_ms: 5000,
        max_passes: 4,
        log_bytes: 1024,
        cancellation: &cancellation,
    };
    let wasm = super::guest(&ResponseHead {
        status: 200,
        headers: vec![],
        body: b"must not deploy".to_vec(),
        streaming: false,
    });
    let error = real
        .deployer
        .prepare_application(
            &manifest("quotes"),
            &wasm,
            &DeployOptions {
                version: VersionId("v1".into()),
                tenant: "acme".into(),
                init_sandbox: Default::default(),
            },
            &context,
        )
        .await
        .err()
        .expect("required preparation refusal must terminate deployment");
    assert!(
        matches!(
            error.downcast_ref::<isoplane_prepare::PrepareError>(),
            Some(isoplane_prepare::PrepareError::Cancelled)
        ),
        "the selected provider's typed refusal must reach the deployment caller"
    );
    isoplane_test_support::assert_empty_directory(
        &directory.path().join("registry"),
        "refused preparation must not publish artifacts",
    );
    drop(real);
    tokio::task::spawn_blocking(move || drop(node))
        .await
        .expect("node shutdown");
}

#[tokio::test]
async fn source_admission_rejects_invalid_input_before_calling_preparation() {
    let directory = crate::common::private_directory().expect("owned registry");
    let deployer = isoplaned::deploy::Deployer::open(directory.path()).expect("registry");
    let engine = isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
        max_instances: 4,
        max_memory_bytes: 4 << 20,
        ..Default::default()
    })
    .expect("engine");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let context = PreparationContext {
        network: crate::common::preparation::network("acme", &manifest("quotes")),
        runtime_configuration: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: &engine,
        provider: &Refusal,
        runtime: RuntimeProfile::raw_wasm(),
        occupancy_ms: 1000,
        wall_ms: 5000,
        max_passes: 4,
        log_bytes: 1024,
        cancellation: &cancellation,
    };
    let error = deployer
        .prepare_application(
            &manifest("quotes"),
            b"not wasm",
            &DeployOptions {
                version: VersionId("v1".into()),
                tenant: "acme".into(),
                init_sandbox: Default::default(),
            },
            &context,
        )
        .await
        .err()
        .expect("invalid source must fail admission");
    assert!(
        matches!(
            error.downcast_ref::<isoplane_control::AdmissionError>(),
            Some(isoplane_control::AdmissionError::Rejected(_))
        ),
        "source rejection must happen before the provider can return its cancellation error"
    );
}
