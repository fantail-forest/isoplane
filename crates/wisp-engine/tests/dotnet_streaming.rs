//! Rejected host response creation is observed through the real managed SDK.

#[path = "common/bindings.rs"]
mod bindings;

use bindings::{Recorder, TestBindings};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_abi::result::ResultRecord;
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, Instance, InstanceOps, Template, WispEngine,
};

#[test]
fn the_managed_sdk_rejects_a_refused_response_handle_before_exposing_a_writer() {
    let (engine, template) = runtime();
    let recorder = Recorder::default();
    recorder.reject_next_response.store(true, Ordering::Relaxed);
    let mut instance = engine
        .activate(template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the response guard fixture must activate");
    let outcome = instance.dispatch(&request("/refused"), budget());
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the guest must catch refused response creation: {outcome:?}"
    );
    let responses = recorder.responses.lock();
    assert_eq!(
        responses.len(),
        1,
        "only the fallback response must be accepted"
    );
    assert_eq!(
        responses[0].body, b"creation-rejected",
        "the real managed SDK must throw for the refused handle"
    );
}

fn runtime() -> &'static (WispEngine, Template) {
    static RUNTIME: OnceLock<(WispEngine, Template)> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 2,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the response guard fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_streaming"))
            .expect("the response guard fixture must load");
        (engine, template)
    })
}

fn request(path: &str) -> DispatchRecord {
    DispatchRecord {
        dispatch_id: 1,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: format!("https://response.test{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1,
    }
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

#[test]
fn a_pending_core_response_write_blocks_concurrent_writes_and_completion_until_acknowledged() {
    let (engine, template) = runtime();
    let recorder = Recorder::default();
    let mut instance: Instance = engine
        .activate(template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the pending response fixture must activate");
    let outcome = instance.dispatch(&request("/pending"), budget());
    assert_eq!(outcome.completion, Completion::Parked, "the guest must await the first write acknowledgement after observing both guards: {outcome:?}");
    assert_eq!(
        recorder
            .calls
            .lock()
            .iter()
            .filter(|call| call.starts_with("resp_write("))
            .count(),
        1,
        "the rejected concurrent write must never enter the host"
    );
    assert!(
        !recorder
            .calls
            .lock()
            .iter()
            .any(|call| call.starts_with("resp_finish(")),
        "completion while a write is pending must not finish the host response"
    );
    let outcome = instance.wake(1, &ResultRecord::Ok(vec![1]), budget());
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "a full-write acknowledgement must resume and complete the guarded response: {outcome:?}"
    );
    let calls = recorder.calls.lock();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("resp_write("))
            .count(),
        1,
        "acknowledgement must not replay the refused write"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("resp_finish("))
            .count(),
        1,
        "the runtime must finish the response exactly once after acknowledgement"
    );
}
