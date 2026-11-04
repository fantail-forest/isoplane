//! The frozen dispatch-slice clock, and import buffers copied in both
//! directions across the guest boundary.

mod common;
#[path = "common/dispatch.rs"]
mod dispatch;

use std::sync::atomic::Ordering;

use common::*;
use dispatch::http_dispatch;
use isoplane_engine::{Completion, InstanceOps, OccupancyBudget};

fn read_u64(m: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
}

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "isoplane" "{name}" (func ${name} {params} {results}))"#)
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

    i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    let (a, b, c) = {
        let m = i.memory();
        (read_u64(m, 0), read_u64(m, 8), read_u64(m, 16))
    };
    assert_eq!(a, b, "the clock advanced mid-slice");
    assert_eq!(b, c, "the clock advanced mid-slice");

    i.dispatch(&http_dispatch(2), OccupancyBudget::default());
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
fn a_future_returning_import_resamples_the_binding_clock() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}\n{}",
        import("clock_now_coarse", "", "(result i64)"),
        import("log_emit", "(param i32 i32 i32)", ""),
        import("http_fetch", "(param i32 i32)", "(result i64)"),
    );
    let wasm = guest(
        4,
        &imports,
        "",
        r#"
    (i64.store (i32.const 0) (call $clock_now_coarse))
    (call $log_emit (i32.const 1) (i32.const 2048) (i32.const 4))
    (i64.store (i32.const 8) (call $clock_now_coarse))
    (drop (call $http_fetch (i32.const 2048) (i32.const 4)))
    (i64.store (i32.const 16) (call $clock_now_coarse))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .expect("activation");
    i.dispatch(&http_dispatch(1), OccupancyBudget::default());

    let m = i.memory();
    let (a, b, c) = (read_u64(m, 0), read_u64(m, 8), read_u64(m, 16));
    assert_eq!(
        a, b,
        "a synchronous import must not advance the frozen clock, or log_emit in a loop is a \
         stopwatch"
    );
    assert!(
        c > b,
        "the synthetic binding advances on sampling; the future import must resample it ({b} -> {c})"
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("log_emit")),
        "log_emit reached the host: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("http_fetch")),
        "http_fetch reached the host: {calls:?}"
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
        import("http_fetch", "(param i32 i32)", "(result i64)"),
        import("entropy_fill", "(param i32 i32)", ""),
    );
    let wasm = guest(
        4,
        &imports,
        "",
        r#"
    (i64.store (i32.const 2048) (i64.const 1234567890))
    (i64.store (i32.const 2056) (i64.const 0))
    (drop (call $http_fetch (i32.const 2048) (i32.const 12)))
    (call $entropy_fill (i32.const 3072) (i32.const 32))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .expect("activation");
    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        out.completion,
        Completion::Done,
        "both imports complete: {:?}",
        out.trap
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.contains(&"http_fetch(12 bytes)".to_string()),
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
        &import("http_fetch", "(param i32 i32)", "(result i64)"),
        "",
        "(drop (call $http_fetch (i32.const 1000000) (i32.const 4096))) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    let trap = out.trap.expect("a buffer outside linear memory must trap");
    assert!(
        trap.message.contains("outside"),
        "the trap must say the buffer was outside linear memory: {trap:?}"
    );
}

#[test]
fn denied_entropy_traps_before_returning_bytes_to_the_guest() {
    assert_entropy_failure(None);
}

#[test]
fn an_inexact_entropy_fill_traps_before_returning_bytes_to_the_guest() {
    for reported_bytes in [0, 31, 33] {
        assert_entropy_failure(Some(reported_bytes));
    }
}

fn assert_entropy_failure(reported_bytes: Option<i64>) {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("entropy_fill", "(param i32 i32)", ""),
        "",
        r#"
        (memory.fill (i32.const 3072) (i32.const 165) (i32.const 32))
        (call $entropy_fill (i32.const 3072) (i32.const 32))
        (i32.store (i32.const 3104) (i32.const 1))
        (i64.const 0)"#,
        DONE,
    );
    let t = e
        .load_template(&wasm)
        .expect("the entropy fixture must load");
    let mut b = TestBindings::new(Recorder::default());
    b.allow_fill = reported_bytes.is_some();
    b.full_length = reported_bytes;
    let mut i = e
        .activate(&t, Box::new(b))
        .expect("the entropy fixture must activate");
    let outcome = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        outcome.completion,
        Completion::Trapped,
        "an unsuccessful entropy draw must trap, never return predictable bytes"
    );
    assert_eq!(
        &i.memory()[3072..3104],
        &[165u8; 32],
        "an unsuccessful entropy draw must not overwrite the guest destination"
    );
    assert_eq!(
        &i.memory()[3104..3108],
        &[0u8; 4],
        "guest execution must not continue after an unsuccessful entropy draw"
    );
}
