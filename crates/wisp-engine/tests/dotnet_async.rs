//! Managed thread-pool continuations across a destroyed Wasmtime Store.

#[path = "common/bindings.rs"]
mod bindings;

use bindings::{Recorder, TestBindings};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_abi::result::ResultRecord;
use wisp_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, WispEngine};

#[test]
fn an_uncaptured_managed_continuation_resumes_on_another_worker_after_store_destruction() {
    let wasm = wisp_fixtures::dotnet("wisp_guest_dotnet_async");
    let engine = WispEngine::new(EngineConfig {
        max_instances: 2,
        max_memory_bytes: 256 << 20,
        guard_size_bytes: 64 << 20,
        ..EngineConfig::default()
    })
    .expect("the managed fixture engine must initialize");
    let template = engine
        .load_template(&wasm)
        .expect("the managed fixture must load");
    let mut instance = engine
        .activate(&template, Box::new(TestBindings::new(Recorder::default())))
        .expect("the managed fixture must activate");
    let dispatch = DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/async".into(),
            path: "/async".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1,
    };
    assert_eq!(
        instance.dispatch(&dispatch, budget()).completion,
        Completion::Parked,
        "managed thread-pool work must reach the host future before parking"
    );
    let image = instance
        .capture_mutable_state()
        .expect("a returned guest must be capturable");
    let memory = instance.memory().to_vec();
    drop(instance);

    let completion = std::thread::spawn(move || {
        let mut restored = engine
            .activate(&template, Box::new(TestBindings::new(Recorder::default())))
            .expect("the captured guest must activate on another worker");
        restored
            .restore_mutable_state(&image)
            .expect("the guest globals must restore");
        restored.memory().copy_from_slice(&memory);
        restored
            .wake(1, &ResultRecord::Ok(vec![0]), budget())
            .completion
    })
    .join()
    .expect("the worker must not panic");
    assert_eq!(
        completion,
        Completion::Done,
        "ConfigureAwait(false) and subsequent thread-pool work must finish after Store destruction"
    );
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}
