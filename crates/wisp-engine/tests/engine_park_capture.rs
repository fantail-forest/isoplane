//! Park state, hibernation capture and restore, and the pre-capture GC hint.

mod common;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;
use wisp_engine::{Completion, CpuBudget, DispatchOutcome, Instance, InstanceOps};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

/// `PollStatus::Pending`.
const PENDING: &str = "(i64.const 1)";

/// A `DispatchRecord` with nothing interesting in it.
fn http_dispatch(id: u64) -> wisp_abi::dispatch::DispatchRecord {
    use wisp_abi::dispatch::*;
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/x".into(),
            path: "/x".into(),
            query: vec![],
            headers: vec![("host".into(), "example.test".into())],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1_700_000_000_000,
    }
}

// ---------------------------------------------------------------------------
// park state and capture
// ---------------------------------------------------------------------------

#[test]
fn an_instance_is_parked_only_after_returning_pending() {
    let e = test_engine();
    let wasm = guest(4, "", "", PENDING, DONE);
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    assert!(!i.is_parked(), "a fresh instance is not parked");

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.status,
        PollStatus::Pending,
        "the fixture returns pending"
    );
    assert!(i.is_parked(), "an instance that returned pending is parked");

    i.wake(1, &ResultRecord::Ok(vec![]), CpuBudget::http());
    assert!(
        !i.is_parked(),
        "an instance that returned done is not parked"
    );

    let wasm = guest(4, "", "", "(unreachable)", DONE);
    let t2 = e.load_template(&wasm).expect("template");
    let mut j = activate(&e, &t2);
    j.dispatch(&http_dispatch(1), CpuBudget::http());
    assert!(
        !j.is_parked(),
        "a trapped instance is never parked however empty its stack"
    );
}

#[test]
fn capture_and_restore_round_trip_mutable_globals() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
  (global $a (export "g_a") (mut i64) (i64.const 100))
  (global $b (export "g_b") (mut i32) (i32.const 200))
  (func $advance (export "advance")
    (global.set $a (i64.add (global.get $a) (i64.const 5)))
    (global.set $b (i32.add (global.get $b) (i32.const 7))))"#,
        "(call $advance) (i64.const 1)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let mut i = activate(&e, &t);
    for n in 0..3 {
        i.dispatch(&http_dispatch(n), CpuBudget::http());
    }
    let img = i.capture_mutable_state().expect("capture");
    assert_eq!(img.memory_pages, 4, "the captured size is the fixture's");

    let mut j = activate(&e, &t);
    let fresh = j.capture_mutable_state().expect("capture");
    assert_ne!(
        format!("{:?}", fresh.globals),
        format!("{:?}", img.globals),
        "the dispatches must have moved the globals"
    );
    j.restore_mutable_state(&img).expect("restore");
    assert_eq!(
        format!("{:?}", j.capture_mutable_state().expect("capture").globals),
        format!("{:?}", img.globals),
        "restore must put the globals exactly where the capture found them"
    );
}

// ---------------------------------------------------------------------------
// the pre-capture GC hint
// ---------------------------------------------------------------------------

#[test]
fn gc_hint_reaches_the_guest_and_leaves_a_parked_instance_parked() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
  (func (export "wisp_gc_hint") (param $pressure i32)
    (i32.store (i32.const 2048) (local.get $pressure))
    (i32.store (i32.const 2052) (i32.add (i32.load (i32.const 2052)) (i32.const 1))))"#,
        PENDING,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    assert!(t.has_gc_hint(), "the fixture exports the hint");

    let mut i = activate(&e, &t);
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert!(i.is_parked(), "the fixture parks");

    let out = i.gc_hint(255);
    assert_eq!(
        out.completion,
        Completion::Done,
        "the hint completes: {:?}",
        out.trap
    );

    let m = i.memory();
    assert_eq!(read_u32(m, 2048), 255, "the pressure reached the guest");
    assert_eq!(read_u32(m, 2052), 1, "the hint ran once");

    assert!(
        i.is_parked(),
        "the hint is called immediately before capture and must not clear the park state"
    );
    i.capture_mutable_state()
        .expect("a hinted, parked instance must still be capturable");
}

#[test]
fn gc_hint_is_optional() {
    let e = test_engine();
    let t = e
        .load_template(&guest(4, "", "", PENDING, DONE))
        .expect("template");
    assert!(!t.has_gc_hint(), "the fixture exports no hint");
    let mut i = activate(&e, &t);
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        i.gc_hint(255).completion,
        Completion::Done,
        "a guest without the export is correct, just stores a larger delta"
    );
    assert!(i.is_parked(), "the no-op hint leaves the instance parked");
}

#[test]
fn aspnet_request_scopes_remain_isolated_after_restoring_guest_state() {
    let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        max_instances: 2,
        max_memory_bytes: 128 << 20,
        guard_size_bytes: 64 << 20,
        ..Default::default()
    })
    .expect("the ASP.NET engine must construct");
    let template = engine
        .load_template(&wisp_fixtures::dotnet("wisp_guest_aspnet"))
        .expect("the packaged ASP.NET guest must load");
    let recorder = Recorder::default();
    let mut instance = engine
        .activate(&template, Box::new(TestBindings::new(recorder.clone())))
        .expect("ASP.NET instance");
    let mut request = http_dispatch(1);
    if let wisp_abi::dispatch::DispatchKind::Http(http) = &mut request.kind {
        http.path = "/scope".into();
        http.url = "http://example.test/scope".into();
    }
    let budget = CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    };
    for _ in 0..2 {
        let result = instance.dispatch(&request, budget);
        let result = acknowledge_response_writes(&mut instance, &recorder, result, budget);
        assert!(
            matches!(result.status, PollStatus::Done { .. }),
            "a native ASP.NET request must dispose its scope before the next request: {result:?}"
        );
    }
    let image = instance
        .capture_mutable_state()
        .expect("ASP.NET state must be capturable at the empty-stack response boundary");
    let memory = instance.memory().to_vec();
    drop(instance);
    let mut restored = engine
        .activate(&template, Box::new(TestBindings::new(recorder.clone())))
        .expect("restored ASP.NET instance");
    restored
        .restore_mutable_state(&image)
        .expect("ASP.NET state must restore in a new Store");
    restored.memory().copy_from_slice(&memory);
    let result = restored.dispatch(&request, budget);
    let result = acknowledge_response_writes(&mut restored, &recorder, result, budget);
    assert!(
        matches!(result.status, PollStatus::Done { .. }),
        "restored ASP.NET state must not retain a completed request scope: {result:?}"
    );
}

#[test]
fn aspnet_cancellation_after_restore_disposes_the_request_scope() {
    let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        max_instances: 2,
        max_memory_bytes: 128 << 20,
        guard_size_bytes: 64 << 20,
        ..Default::default()
    })
    .expect("the ASP.NET engine must construct");
    let template = engine
        .load_template(&wisp_fixtures::dotnet("wisp_guest_aspnet"))
        .expect("the packaged ASP.NET guest must load");
    let recorder = Recorder::default();
    let mut instance = engine
        .activate(&template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the cancellation guest must activate");
    let mut request = http_dispatch(1);
    request.cancel_future = 42;
    if let wisp_abi::dispatch::DispatchKind::Http(http) = &mut request.kind {
        http.path = "/cancel".into();
        http.url = "http://example.test/cancel".into();
    }
    let budget = CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    };
    assert_eq!(
        instance.dispatch(&request, budget).completion,
        Completion::Parked,
        "ASP.NET must await the host cancellation signal with an empty stack"
    );
    let image = instance
        .capture_mutable_state()
        .expect("the suspended request state must capture");
    let memory = instance.memory().to_vec();
    drop(instance);
    let mut restored = engine
        .activate(&template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the cancellation guest must reactivate");
    restored
        .restore_mutable_state(&image)
        .expect("the suspended request state must restore");
    restored.memory().copy_from_slice(&memory);
    let result = restored.wake(
        42,
        &ResultRecord::Err(wisp_abi::result::FutureError::Cancelled),
        budget,
    );
    let result = acknowledge_response_writes(&mut restored, &recorder, result, budget);
    assert_eq!(
        result.completion,
        Completion::Done,
        "ASP.NET middleware must handle host cancellation after restore: {result:?}"
    );
    assert_eq!(
        recorder
            .responses
            .lock()
            .last()
            .expect("a cancellation response")
            .status,
        499,
        "the restored request must observe cancellation through RequestAborted"
    );
    if let wisp_abi::dispatch::DispatchKind::Http(http) = &mut request.kind {
        http.path = "/scope".into();
        http.url = "http://example.test/scope".into();
    }
    request.dispatch_id += 1;
    request.cancel_future += 1;
    let result = restored.dispatch(&request, budget);
    let result = acknowledge_response_writes(&mut restored, &recorder, result, budget);
    assert_eq!(
        result.completion,
        Completion::Done,
        "a cancelled request must release its DI scope before reuse: {result:?}"
    );
    assert_eq!(
        recorder
            .responses
            .lock()
            .last()
            .expect("the next response")
            .body,
        b"isolated",
        "the next request must own a fresh scope"
    );
}

fn acknowledge_response_writes(
    instance: &mut Instance,
    recorder: &Recorder,
    mut outcome: DispatchOutcome,
    budget: CpuBudget,
) -> DispatchOutcome {
    for _ in 0..8 {
        if outcome.completion != Completion::Parked {
            return outcome;
        }
        let (future, bytes) = recorder.response_writes.lock().pop_front().expect("the parked ASP.NET fixture must be awaiting an explicit response write acknowledgement");
        outcome = instance.wake(
            future,
            &ResultRecord::Ok(postcard::to_allocvec(&bytes).expect("write byte count")),
            budget,
        );
    }
    panic!("the small ASP.NET response failed to finish within its expected write count");
}
