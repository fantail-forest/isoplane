//! The frozen dispatch-slice clock, and import buffers copied in both
//! directions across the guest boundary.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use wisp_engine::{Completion, CpuBudget, InstanceOps};

fn read_u64(m: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
}

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "wisp" "{name}" (func ${name} {params} {results}))"#)
}

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
// the frozen clock
// ---------------------------------------------------------------------------

#[test]
fn the_coarse_clock_does_not_advance_within_a_dispatch_slice() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("clock_now_coarse", "", "(result i64)"),
        "",
        r#"
    (i64.store (i32.const 0) (call $clock_now_coarse))
    (drop (call $burn (i32.const 200000)))
    (i64.store (i32.const 8) (call $clock_now_coarse))
    (drop (call $burn (i32.const 200000)))
    (i64.store (i32.const 16) (call $clock_now_coarse))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .expect("activation");

    i.dispatch(&http_dispatch(1), CpuBudget::http());
    let (a, b, c) = {
        let m = i.memory();
        (read_u64(m, 0), read_u64(m, 8), read_u64(m, 16))
    };
    assert_eq!(a, b, "the clock advanced mid-slice");
    assert_eq!(b, c, "the clock advanced mid-slice");

    i.dispatch(&http_dispatch(2), CpuBudget::http());
    let (d, e2, f) = {
        let m = i.memory();
        (read_u64(m, 0), read_u64(m, 8), read_u64(m, 16))
    };
    assert_eq!(d, e2, "the clock advanced mid-slice");
    assert_eq!(e2, f, "the clock advanced mid-slice");
    assert!(
        d > a,
        "a new slice must see a fresh clock ({a} -> {d}), or the freeze test is vacuous"
    );
    assert!(
        rec.clock_reads.load(Ordering::Relaxed) >= 2,
        "the host clock must have been sampled once per slice"
    );
}

#[test]
fn a_future_returning_import_is_the_only_thing_that_advances_the_clock() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}\n{}",
        import("clock_now_coarse", "", "(result i64)"),
        import("log_emit", "(param i32 i32 i32)", ""),
        import("kv_get", "(param i32 i32)", "(result i64)"),
    );
    let wasm = guest(
        4,
        &imports,
        "",
        r#"
    (i64.store (i32.const 0) (call $clock_now_coarse))
    (call $log_emit (i32.const 1) (i32.const 2048) (i32.const 4))
    (i64.store (i32.const 8) (call $clock_now_coarse))
    (drop (call $kv_get (i32.const 2048) (i32.const 4)))
    (i64.store (i32.const 16) (call $clock_now_coarse))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .expect("activation");
    i.dispatch(&http_dispatch(1), CpuBudget::http());

    let m = i.memory();
    let (a, b, c) = (read_u64(m, 0), read_u64(m, 8), read_u64(m, 16));
    assert_eq!(
        a, b,
        "a synchronous import must not advance the frozen clock, or log_emit in a loop is a \
         stopwatch"
    );
    assert!(
        c > b,
        "a future-returning import is a host-call boundary and advances the clock ({b} -> {c})"
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("log_emit")),
        "log_emit reached the host: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("kv_get")),
        "kv_get reached the host: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("clock_now_coarse")),
        "the frozen clock is engine-owned and must never reach a binding: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// buffers, in both directions
// ---------------------------------------------------------------------------

#[test]
fn guest_buffers_are_copied_out_and_out_buffers_are_copied_in() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}",
        import("kv_get", "(param i32 i32)", "(result i64)"),
        import("entropy_fill", "(param i32 i32)", ""),
    );
    let wasm = guest(
        4,
        &imports,
        "",
        r#"
    (i64.store (i32.const 2048) (i64.const 1234567890))
    (i64.store (i32.const 2056) (i64.const 0))
    (drop (call $kv_get (i32.const 2048) (i32.const 12)))
    (call $entropy_fill (i32.const 3072) (i32.const 32))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .expect("activation");
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Done,
        "both imports complete: {:?}",
        out.trap
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.contains(&"kv_get(12 bytes)".to_string()),
        "the guest's 12 bytes were copied out before the call: {calls:?}"
    );
    assert!(
        calls.contains(&"entropy_fill(0 bytes)".to_string()),
        "an out-buffer import reaches the host with an empty payload, never a pointer into \
         guest memory: {calls:?}"
    );
    assert_eq!(
        &i.memory()[3072..3104],
        &[0xABu8; 32],
        "the host's fill was copied into guest memory"
    );
}

#[test]
fn an_out_of_bounds_import_buffer_traps_rather_than_reading_host_memory() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("kv_get", "(param i32 i32)", "(result i64)"),
        "",
        "(drop (call $kv_get (i32.const 1000000) (i32.const 4096))) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    let trap = out.trap.expect("a buffer outside linear memory must trap");
    assert!(
        trap.message.contains("outside"),
        "the trap must say the buffer was outside linear memory: {trap:?}"
    );
}

#[test]
fn the_default_host_bindings_deny_out_buffer_fills() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("entropy_fill", "(param i32 i32)", ""),
        "",
        "(call $entropy_fill (i32.const 3072) (i32.const 32)) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut b = TestBindings::new(Recorder::default());
    b.allow_fill = false;
    let mut i = e.activate(&t, Box::new(b)).expect("activation");
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        &i.memory()[3072..3104],
        &[0u8; 32],
        "a denied fill must write nothing, so a missing CSPRNG cannot pass as a constant one"
    );
}
