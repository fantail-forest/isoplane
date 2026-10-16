//! The two-phase kill (grace and the hard deadline) and the wall-clock
//! independent per-slice wall-clock backstop.

mod common;
#[path = "common/dispatch.rs"]
mod dispatch;

use common::*;
use dispatch::http_dispatch;
use isoplane_abi::result::ResultRecord;
use isoplane_engine::{Completion, InstanceOps, KillPhase, OccupancyBudget, TrapKind};

fn read_u64(m: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
}

fn budget(occupancy_ms: u32) -> OccupancyBudget {
    OccupancyBudget {
        occupancy_ms,
        slice_wall_ms_cap: 30_000,
        spent_ns: 0,
    }
}

const SPIN: &str = "(func $spin (loop $l (br $l)))";

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "isoplane" "{name}" (func ${name} {params} {results}))"#)
}

// ---------------------------------------------------------------------------
// the two-phase kill
// ---------------------------------------------------------------------------

/// The guest marks memory before it spins and would mark it again on the way
/// out; a hard kill is not unwound, so the second marker never appears.
#[test]
fn an_uncooperative_spinner_is_hard_killed_and_discarded() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        SPIN,
        r#"
    (i64.store (i32.const 3000) (i64.const 1))   ;; entered
    (call $spin)
    (i64.store (i32.const 3008) (i64.const 1))   ;; the `finally`
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let b = budget(20);
    let out = i.dispatch(&http_dispatch(1), b);

    let trap = out.trap.as_ref().expect("a spinner must trap");
    assert_eq!(
        trap.kind,
        TrapKind::BudgetExceeded,
        "the trap must be classified as a budget kill: {trap:?}"
    );
    assert_eq!(
        out.completion,
        Completion::BudgetKill,
        "a spinner takes the hard kill"
    );
    assert_eq!(out.kill, Some(KillPhase::Hard), "the phase is hard");
    assert!(
        out.occupancy_ns >= b.hard_ns(),
        "the kill lands at B+G, not before: {} ns against {} ns",
        out.occupancy_ns,
        b.hard_ns()
    );
    assert!(
        out.overage_ns >= b.grace_ns(),
        "a dispatch that ran the whole grace window is recorded at least G of overage"
    );

    let (entered, cleaned_up) = {
        let m = i.memory();
        (read_u64(m, 3000), read_u64(m, 3008))
    };
    assert_eq!(entered, 1, "the guest must have been running");
    assert_eq!(
        cleaned_up, 0,
        "no `finally` may run after a hard budget kill"
    );

    assert!(!i.is_parked(), "a discarded instance is not parked");
    assert!(
        !i.is_capture_eligible(),
        "a discarded instance is never a capture candidate"
    );
    assert!(
        i.reset().is_err(),
        "a hard-killed instance must not be reset back into the pool"
    );
    assert!(
        i.capture_mutable_state().is_err(),
        "deltas are never captured from a discarded instance"
    );
}

#[test]
fn a_discarded_instance_refuses_the_next_call() {
    let e = test_engine();
    let wasm = guest(4, "", SPIN, "(call $spin) (i64.const 0)", DONE);
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let b = budget(5);

    let out = i.dispatch(&http_dispatch(1), b);
    assert_eq!(
        out.completion,
        Completion::BudgetKill,
        "the spinner must be hard-killed"
    );

    let refused = i.dispatch(&http_dispatch(2), b);
    assert_eq!(
        refused.completion,
        Completion::Trapped,
        "a call into a discarded instance must not run the guest"
    );
    let trap = refused.trap.expect("the refusal is reported as a trap");
    assert_eq!(
        trap.kind,
        TrapKind::Discarded,
        "the refusal says the instance was discarded: {trap:?}"
    );
    assert_eq!(
        refused.occupancy_ns, 0,
        "a refused call bills nothing, since nothing ran"
    );

    let woke = i.wake(1, &ResultRecord::Ok(vec![]), b);
    assert_eq!(
        woke.trap.map(|t| t.kind),
        Some(TrapKind::Discarded),
        "a wake into a discarded instance is refused too"
    );
}

/// The runtime layer's unsafe-to-trap bit is the one mechanism that could
/// turn the hard deadline into a negotiation; a pinned bit buys exactly G.
#[test]
fn a_spinner_with_the_unsafe_to_trap_bit_pinned_is_still_hard_killed() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("unsafe_to_trap", "(param i32)", ""),
        SPIN,
        "(call $unsafe_to_trap (i32.const 1)) (call $spin) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let b = budget(10);

    let out = i.dispatch(&http_dispatch(1), b);
    assert_eq!(
        out.completion,
        Completion::BudgetKill,
        "a pinned bit does not prevent the hard kill"
    );
    assert_eq!(out.kill, Some(KillPhase::Hard), "the phase is hard");
    assert!(
        out.occupancy_ns >= b.hard_ns(),
        "the kill still lands at B+G, not before"
    );
    assert!(
        !i.is_capture_eligible(),
        "a hard-killed instance is never a capture candidate"
    );
}

#[test]
fn a_guest_abort_is_a_guest_panic_not_a_sandbox_violation() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("abort", "(param i32 i32)", ""),
        "",
        r#"(call $abort (i32.const 0) (i32.const 5)) (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    i.memory()[0..5].copy_from_slice(b"boom!");

    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    let trap = out.trap.expect("abort traps");
    assert_eq!(
        trap.kind,
        TrapKind::GuestPanic,
        "abort is the guest's own fault: {trap:?}"
    );
    assert!(
        trap.message.contains("boom!"),
        "the abort message reaches telemetry: {}",
        trap.message
    );
    assert!(
        i.reset().is_err(),
        "a trapped instance is discarded and cannot be reset"
    );
}

#[test]
fn a_bounds_violation_is_classified_as_a_sandbox_trap() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        "",
        "(drop (i64.load (i32.const 1000000000))) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        out.trap.as_ref().map(|t| t.kind),
        Some(TrapKind::Sandbox),
        "an out-of-bounds load is a sandbox trap: {:?}",
        out.trap
    );
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "a sandbox trap discards the instance"
    );
}

#[test]
fn a_failed_isoplane_alloc_is_out_of_memory() {
    let e = test_engine();
    let src = r#"
(module
  (memory (export "memory") 1)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    let t = e
        .load_template(&wat::parse_str(src).expect("wat"))
        .expect("template");
    let mut i = activate(&e, &t);
    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        out.trap.as_ref().map(|t| t.kind),
        Some(TrapKind::OutOfMemory),
        "an allocator that returns 0 is out of memory: {:?}",
        out.trap
    );
}

// ---------------------------------------------------------------------------
// the wall-clock backstop
// ---------------------------------------------------------------------------

#[test]
fn the_wall_clock_backstop_reports_its_distinct_cap() {
    let e = test_engine();
    let wasm = guest(4, "", SPIN, "(call $spin) (i64.const 0)", DONE);
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let out = i.dispatch(
        &http_dispatch(1),
        OccupancyBudget {
            occupancy_ms: 600_000,
            slice_wall_ms_cap: 40,
            spent_ns: 0,
        },
    );
    let trap = out.trap.as_ref().expect("the backstop must trip");
    assert_eq!(
        trap.kind,
        TrapKind::BudgetExceeded,
        "the tenant sees the same abrupt termination: {trap:?}"
    );
    assert!(
        trap.message.contains("wall-clock backstop"),
        "the message must distinguish the backstop from occupancy exhaustion: {}",
        trap.message
    );
    assert!(
        out.wall_ns >= 40_000_000,
        "the backstop fires at the cap, not before: {} ns",
        out.wall_ns
    );
}
