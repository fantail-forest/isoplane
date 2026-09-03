//! Park state, hibernation capture and restore, and the pre-capture GC hint.

mod common;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;
use wisp_engine::{Completion, CpuBudget, InstanceOps};

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
