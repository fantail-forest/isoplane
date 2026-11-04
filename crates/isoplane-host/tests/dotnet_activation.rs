//! Asynchronous managed activation through the production cell and timer reactor.

use isoplane_harness::logging::GuestLogs;
mod common;

use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{IsoplaneEngine, PlatformFile, PlatformFiles};
use isoplane_host::activator::EngineActivator;
use isoplane_host::bindings::RuntimeConfiguration;
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::ModuleId;
use std::sync::Arc;
use std::time::Duration;

fn activation_cell(settings: &[u8]) -> (Arc<Cell>, ModuleId) {
    let engine = Arc::new(
        IsoplaneEngine::new(isoplane_fixtures::engine_config(4))
            .expect("the managed activation engine must start"),
    );
    let module = ModuleId::new("managed-activation", "v1");
    let files = isoplane_fixtures::managed_probe("isoplane_guest_dotnet_activation")
        .into_iter()
        .map(|file| PlatformFile {
            path: file.path,
            bytes: file.bytes.into(),
            modified_ns: 0,
        })
        .collect();
    let template = isoplane_fixtures::load_compiled_runtime(&engine)
        .with_application_files(PlatformFiles::new(files).expect("valid activation package files"))
        .expect("bind the managed activation probe");
    let activator = EngineActivator::new(engine, template, module.clone());
    let cell = common::rig_resolving(
        CellConfig::for_test("managed-activation"),
        "93.184.216.34:80",
    )
    .0;
    cell.register_module(
        "tenant",
        Arc::new(activator),
        common::manifest("managed-activation"),
    )
    .expect("the activation module must register");
    cell.set_runtime_configuration(
        "tenant",
        &module,
        // The guest is not pre-initialized here, so its initializer runs before
        // the first dispatch and reads `probe`.
        RuntimeConfiguration::from_json(settings)
            .expect("the activation fixture settings must be valid"),
    )
    .expect("the activation fixture must receive its settings");

    (cell, module)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interpreted_activation_awaits_its_timer_before_dispatch_and_does_not_repeat() {
    let (cell, module) = activation_cell(br#"{"activation-timer":"enabled","probe":"unprepared"}"#);
    let mut first_instance = None;
    for _ in 0..2 {
        let request = HttpRequest {
            method: "GET".into(),
            url: "https://activation.test/activations".into(),
            path: "/activations".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        let response = cell
            .dispatch(
                &module,
                Target::Handler {
                    route: "/activations".into(),
                },
                cell.http_record(&module, request),
                Duration::from_secs(60),
            )
            .await
            .expect("the asynchronous activation timer must permit dispatch to finish");
        assert_eq!(
            response.status,
            200,
            "activation must complete successfully: {response:?}; logs: {:?}",
            cell.slot(response.instance).map(|slot| slot.ctx.logs())
        );
        assert_eq!(
            response.body, b"1",
            "the request must observe exactly one completed activation after its timer"
        );
        assert_eq!(
            *first_instance.get_or_insert(response.instance),
            response.instance,
            "both requests must exercise the same activated instance"
        );
        tokio::time::timeout(Duration::from_secs(60), async {
            while cell.idle_instances(&module) != Some(1) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the activated guest must return to the reuse pool");
    }
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_activation_discards_the_instance_before_another_request() {
    let (cell, module) = activation_cell(
        br#"{"activation-timer":"enabled","activation-failure":"enabled","probe":"unprepared"}"#,
    );
    let mut previous = None;
    for _ in 0..2 {
        let request = HttpRequest {
            method: "GET".into(),
            url: "https://activation.test/activations".into(),
            path: "/activations".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        let result = cell
            .dispatch(
                &module,
                Target::Handler {
                    route: "/activations".into(),
                },
                cell.http_record(&module, request),
                Duration::from_secs(60),
            )
            .await
            .expect("a failed activation must terminate its request");
        assert_eq!(result.status, 500);
        assert_eq!(result.completion, isoplane_engine::Completion::Trapped);
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if cell.slot(result.instance).is_none() {
                    break;
                }
                assert_ne!(
                    cell.idle_instances(&module),
                    Some(1),
                    "a failed activation must not return to the reuse pool"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed activation must release its instance");
        assert_ne!(previous, Some(result.instance));
        previous = Some(result.instance);
    }
    cell.shutdown();
}
