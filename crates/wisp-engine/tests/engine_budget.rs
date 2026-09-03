//! The two-phase kill (grace and the hard deadline) and the wall-clock
//! backstop that catches what the CPU meter cannot.

mod common;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_engine::{Completion, CpuBudget, InstanceOps, KillPhase, TrapKind};

fn read_u64(m: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
}

fn budget(cpu_ms: u32) -> CpuBudget {
    CpuBudget {
        cpu_ms,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    }
}

const SPIN: &str = "(func $spin (loop $l (br $l)))";

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
// the two-phase kill
// ---------------------------------------------------------------------------

/// The guest marks memory before it spins and would mark it again on the way
/// out; a hard kill is not unwound, so the second marker never appears.
#[test]
fn an_uncooperative_spinner_is_hard_killed_and_tombstoned() {
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
        out.cpu_ns >= b.hard_ns(),
        "the kill lands at B+G, not before: {} ns against {} ns",
        out.cpu_ns,
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

    assert!(!i.is_parked(), "a tombstone is not parked");
    assert!(
        !i.is_capture_eligible(),
        "a tombstone is never a capture candidate"
    );
    assert!(
        i.reset().is_err(),
        "a hard-killed instance must not be reset back into the pool"
    );
    assert!(
        i.capture_mutable_state().is_err(),
        "deltas are never captured from a tombstone"
    );
}

#[test]
fn a_tombstoned_instance_refuses_the_next_call() {
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
        "a call into a tombstone must not run the guest"
    );
    let trap = refused.trap.expect("the refusal is reported as a trap");
    assert_eq!(
        trap.kind,
        TrapKind::Tombstoned,
        "the refusal names the tombstone: {trap:?}"
    );
    assert_eq!(
        refused.cpu_ns, 0,
        "a refused call bills nothing, since nothing ran"
    );

    let woke = i.wake(1, &ResultRecord::Ok(vec![]), b);
    assert_eq!(
        woke.trap.map(|t| t.kind),
        Some(TrapKind::Tombstoned),
        "a wake into a tombstone is refused too"
    );
}

/// The identical spin loop, plus a `budget_remaining_ns` check every 1024
/// iterations. A runtime layer does the same at its allocation slow path, GC
/// entry and task switch.
#[test]
fn a_dispatch_that_yields_during_grace_takes_a_clean_kill() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("budget_remaining_ns", "", "(result i64)"),
        r#"
  (func $cooperate (result i32)
    (local $n i32) (local $x i64)
    (local.set $x (i64.const 88172645463325252))
    (block $out
      (loop $l
        (local.set $n (i32.const 0))
        (block $ib (loop $il
          (br_if $ib (i32.ge_u (local.get $n) (i32.const 1024)))
          (local.set $x (i64.xor (local.get $x) (i64.shl (local.get $x) (i64.const 13))))
          (local.set $x (i64.xor (local.get $x) (i64.shr_u (local.get $x) (i64.const 7))))
          (local.set $x (i64.xor (local.get $x) (i64.shl (local.get $x) (i64.const 17))))
          (local.set $n (i32.add (local.get $n) (i32.const 1)))
          (br $il)))
        (i64.store (i32.const 3016) (local.get $x))
        (br_if $out (i64.le_s (call $budget_remaining_ns) (i64.const 0)))
        (br $l)))
    (i32.const 1))"#,
        r#"
    (i64.store (i32.const 3000) (i64.const 1))          ;; entered
    (drop (call $cooperate))
    (i64.store (i32.const 3008) (i64.const 1))          ;; the `finally`
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let b = budget(20);

    let out = i.dispatch(&http_dispatch(1), b);

    assert!(
        out.trap.is_none(),
        "a clean kill is a return, not a trap: {:?}",
        out.trap
    );
    assert_eq!(
        out.completion,
        Completion::CleanKill,
        "a dispatch that returned inside grace is a clean kill"
    );
    assert_eq!(out.kill, Some(KillPhase::Clean), "the phase is clean");
    assert!(
        out.completion.instance_survives(),
        "the slot is reusable after a clean kill"
    );

    let (entered, cleaned_up) = {
        let m = i.memory();
        (read_u64(m, 3000), read_u64(m, 3008))
    };
    assert_eq!(entered, 1, "the guest must have been running");
    assert_eq!(cleaned_up, 1, "on a clean kill the `finally` runs");
    assert!(
        out.cpu_ns >= b.budget_ns(),
        "the dispatch must have crossed B rather than returned early"
    );
    assert!(out.overage_ns > 0, "the overage CPU is recorded");

    i.reset().expect("the slot is reusable after a clean kill");
    let again = i.dispatch(&http_dispatch(2), b);
    assert_eq!(
        again.completion,
        Completion::CleanKill,
        "the reused slot runs the guest again rather than refusing it"
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
        out.cpu_ns >= b.hard_ns(),
        "the kill still lands at B+G, not before"
    );
    assert!(
        out.grace_deferrals > 0,
        "the pinned bit must have deferred grace entry, or the fixture is not testing it"
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

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
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
        "a trapped instance is tombstoned and cannot be reset"
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
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
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
fn a_failed_wisp_alloc_is_out_of_memory() {
    let e = test_engine();
    let src = r#"
(module
  (memory (export "memory") 1)
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    let t = e
        .load_template(&wat::parse_str(src).expect("wat"))
        .expect("template");
    let mut i = activate(&e, &t);
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
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
        CpuBudget {
            cpu_ms: 600_000,
            wall_ms_cap: 40,
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
        "the message must distinguish the backstop from the CPU meter, since wall time is \
         excluded from execution CPU accounting: {}",
        trap.message
    );
    assert!(
        out.wall_ns >= 40_000_000,
        "the backstop fires at the cap, not before: {} ns",
        out.wall_ns
    );
}
