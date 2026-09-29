//! Asynchronous managed activation through the production cell and timer reactor.

mod common;

use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{EngineConfig, IsoplaneEngine, PlatformFile, PlatformFiles};
use isoplane_fixtures::GuestRuntime;
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_host::activator::EngineActivator;
use isoplane_host::bindings::RuntimeConfiguration;
use isoplane_host::cell::{CellConfig, Target};
use isoplane_host::ModuleId;
use std::sync::Arc;
use std::time::Duration;

async fn activation_finishes_before_dispatch(runtime: GuestRuntime) {
    isoplane_harness::LogCapture::global();
    let GuestRuntime::Mono = runtime;
    let wasm = isoplane_fixtures::mono_runtime();
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 4,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
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
    let template = engine
        .load_template(&wasm)
        .expect("the shared runtime must load")
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
        common::manifest(
            "managed-activation",
            HandlerLifecycle::Reuse,
            Limits::default(),
        ),
    )
    .expect("the activation module must register");
    cell.set_runtime_configuration(
        "tenant",
        &module,
        RuntimeConfiguration::from_json(br#"{"activation-timer":"enabled"}"#)
            .expect("the activation timer fixture setting must be valid"),
    )
    .expect("the activation fixture must receive its timer setting");

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
            cell.slot(response.instance)
                .map(|slot| slot.ctx.emitted_logs())
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
            while cell.idle_instances(&module) != Some((1, 0)) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the activated guest must return to the reuse pool");
    }
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interpreted_activation_awaits_its_timer_before_dispatch_and_does_not_repeat() {
    activation_finishes_before_dispatch(GuestRuntime::Mono).await;
}
