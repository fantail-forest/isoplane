//! Behavioral tests for the sandbox boundary.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, InstanceOps, KillPhase, TrapKind, WispEngine,
};

/// Small engine: 64 slots, 4 MiB ceiling, small guard. The guard is not the
/// 2 GiB default because every test process would otherwise reserve tens of
/// GiB of address space per engine it builds.
fn test_engine() -> WispEngine {
    WispEngine::new(EngineConfig {
        max_instances: 64,
        max_memory_bytes: 4 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
    })
    .expect("engine")
}

fn echo_guest() -> Vec<u8> {
    guest(4, "", "", DONE, DONE)
}

// ---------------------------------------------------------------------------
// §4.3 V1 / invariant 4 — imports are the complete capability surface
// ---------------------------------------------------------------------------

#[test]
fn an_import_outside_the_table_fails_to_load() {
    let e = test_engine();

    // Right module, wrong field.
    let wasm = guest(
        4,
        r#"(import "wisp" "definitely_not_an_import" (func $x (result i64)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("undeclared import must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("definitely_not_an_import"),
        "error must name the offender: {msg}"
    );
    assert!(
        msg.contains("V1"),
        "must cite the rule it is enforcing: {msg}"
    );

    // Right field, wrong module: `wisp` is the only host module (§5.3).
    let wasm = guest(
        4,
        r#"(import "wasi_snapshot_preview1" "fd_write" (func $x (param i32) (result i32)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("a second host module must be rejected");
    assert!(format!("{err:#}").contains("fd_write"));

    // And the declared ones do link.
    let wasm = guest(
        4,
        &import("kv_get", "(param i32 i32)", "(result i64)"),
        "",
        DONE,
        DONE,
    );
    e.load_template(&wasm).expect("a declared import must link");
}

#[test]
fn a_module_missing_a_required_export_cannot_activate() {
    let e = test_engine();
    // Everything except `wisp_wake`.
    let src = r#"
(module
  (memory (export "memory") 1)
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 1024))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0)))"#;
    let t = e.load_template(&wat::parse_str(src).unwrap()).unwrap();
    let err = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .expect_err("a module without wisp_wake is not deployable (§5.2)");
    assert!(format!("{err:#}").contains("wisp_wake"), "{err:#}");
}

// ---------------------------------------------------------------------------
// §6.2 — activation and reset
// ---------------------------------------------------------------------------

#[test]
fn reset_restores_template_contents_byte_exactly() {
    let e = test_engine();
    let t = e.load_template(&echo_guest()).unwrap();
    let image = t.memory_image().to_vec();

    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    assert_eq!(
        i.memory(),
        image.as_slice(),
        "a fresh activation must equal the template"
    );

    // Scribble over a spread of pages, including one far from the others.
    for off in [0usize, 4096, 65536, 30 * 4096, 60 * 4096] {
        i.memory()[off..off + 8].copy_from_slice(&[0xAB; 8]);
    }
    assert_ne!(i.memory(), image.as_slice());
    assert!(
        i.dirty_page_count() >= 5,
        "dirtied {} pages",
        i.dirty_page_count()
    );

    i.reset().unwrap();

    assert_eq!(
        i.memory(),
        image.as_slice(),
        "§6.2: reset restores template contents — byte for byte, not approximately"
    );
    assert_eq!(i.dirty_page_count(), 0);
}

#[test]
fn activation_seals_globals_and_tables_from_the_template() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
  (table (export "t") 4 4 funcref)
  (elem (i32.const 0) $marker)
  (func $marker (result i32) (i32.const 7))
  (global $counter (export "g_counter") (mut i64) (i64.const 41))
  (func (export "bump") (result i64)
    (global.set $counter (i64.add (global.get $counter) (i64.const 1)))
    (global.get $counter))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();

    let baseline = {
        let mut i = e
            .activate(&t, Box::new(TestBindings::new(Recorder::default())))
            .unwrap();
        i.capture_mutable_state().unwrap()
    };

    // Mutate one activation's globals and memory...
    let mut a = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    a.memory()[0..8].copy_from_slice(&[9u8; 8]);
    let mut img = baseline.clone();
    img.globals = img
        .globals
        .iter()
        .map(|g| match g {
            wisp_engine::snapshot::GlobalValue::I64(x) => {
                wisp_engine::snapshot::GlobalValue::I64(x + 1000)
            }
            other => *other,
        })
        .collect();
    a.restore_mutable_state(&img).unwrap();
    let mutated = a.capture_mutable_state().unwrap();
    assert_ne!(
        format!("{:?}", mutated.globals),
        format!("{:?}", baseline.globals),
        "the test must actually have changed something"
    );

    // ...and a fresh activation of the same template must not see any of it.
    // §6.2 step 2: globals and tables are initialized from the template's
    // sealed values, which wasmtime instantiation performs.
    let mut b = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let fresh = b.capture_mutable_state().unwrap();
    assert_eq!(
        format!("{:?}", fresh.globals),
        format!("{:?}", baseline.globals)
    );
    assert_eq!(b.memory(), t.memory_image());
    // The table is re-sealed too: capture would have failed if any slot's
    // nullness differed from the template baseline.
    assert!(fresh.table_mutations.is_empty());

    // And `reset` gets the same seal, since it is a re-instantiation.
    a.reset().unwrap();
    let after = a.capture_mutable_state().unwrap();
    assert_eq!(
        format!("{:?}", after.globals),
        format!("{:?}", baseline.globals)
    );
}

#[test]
fn a_serialized_template_round_trips_and_is_engine_pinned() {
    let e = test_engine();
    let t = e.load_template(&echo_guest()).unwrap();
    let bytes = e.serialize(&t).unwrap();

    // SAFETY: produced by `serialize` on this very engine.
    let back = unsafe { e.deserialize(&bytes) }.unwrap();
    assert_eq!(back.hash, t.hash);
    assert_eq!(back.abi_rev, wisp_abi::ABI_REV);
    assert_eq!(back.memory_image(), t.memory_image());
    assert_eq!(e.artifact_key(&back), e.artifact_key(&t));

    let mut i = e
        .activate(&back, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done
    );

    // §4.4/§9.3: artifacts are content-addressed by (module hash, engine
    // version, µarch). An engine configured differently must refuse them.
    let other = WispEngine::new(EngineConfig {
        max_instances: 8,
        max_memory_bytes: 8 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: false,
        guard_size_bytes: 2 << 20,
    })
    .unwrap();
    let err =
        unsafe { other.deserialize(&bytes) }.expect_err("cross-engine artifact must be refused");
    assert!(format!("{err:#}").contains("engine"), "{err:#}");
}

#[test]
fn deferred_init_units_travel_in_the_template() {
    let e = test_engine();
    let mut wasm = echo_guest();
    // The section `wisp-control` pre-init is expected to emit (§4.4).
    let mut payload = 3u32.to_le_bytes().to_vec();
    for id in [7u32, 11, 13] {
        payload.extend_from_slice(&id.to_le_bytes());
    }
    append_custom_section(&mut wasm, wisp_engine::DEFERRED_INIT_SECTION, &payload);

    let t = e.load_template(&wasm).unwrap();
    assert_eq!(t.deferred_init_units(), &[7, 11, 13]);

    // ...and survive the artifact cache.
    let bytes = e.serialize(&t).unwrap();
    let back = unsafe { e.deserialize(&bytes) }.unwrap();
    assert_eq!(back.deferred_init_units(), &[7, 11, 13]);
}

fn append_custom_section(wasm: &mut Vec<u8>, name: &str, data: &[u8]) {
    fn leb(mut v: u32, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    let mut body = Vec::new();
    leb(name.len() as u32, &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(data);
    wasm.push(0); // custom section id
    leb(body.len() as u32, wasm);
    wasm.extend_from_slice(&body);
}

// ---------------------------------------------------------------------------
// §6.4 / §2.5.8 / C7 — budget kills are abrupt and self-limiting
// ---------------------------------------------------------------------------

/// §6.4/§2.5.8, and the sharper form the spec took in v1.4: an *uncooperative*
/// spinner is killed at **B+G**, not at B, and only then is the kill abrupt.
#[test]
fn an_uncooperative_spinner_is_hard_killed_at_b_plus_g_and_tombstoned() {
    let e = test_engine();
    // A tight back-edge loop: epoch checks are Cranelift-inserted at back-edges.
    // The guest marks memory before it starts spinning and would mark it again
    // on the way out. §2.5.8: a budget kill is "not unwound: no `finally`
    // blocks, no finalizers, no cleanup quantum", so the second marker must
    // never appear.
    let wasm = guest(
        4,
        "",
        r#"
  (global $spins (export "g_spins") (mut i64) (i64.const 0))
  (func $spin (loop $l (global.set $spins (i64.add (global.get $spins) (i64.const 1))) (br $l)))"#,
        r#"
    (i64.store (i32.const 3000) (i64.const 1))   ;; entered
    (call $spin)
    (i64.store (i32.const 3008) (i64.const 1))   ;; "finally" — must never run
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();

    let budget = CpuBudget {
        cpu_ms: 20,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    };
    let out = i.dispatch(&http_dispatch(1), budget);

    let trap = out.trap.as_ref().expect("a spinner must trap");
    assert_eq!(trap.kind, TrapKind::BudgetExceeded, "{trap:?}");
    assert_eq!(trap.kind.tenant_code(), "budget_exceeded");
    assert_eq!(out.completion, Completion::BudgetKill);
    assert!(!out.completion.instance_survives());

    assert_eq!(out.kill, Some(KillPhase::Hard));

    // The kill lands at B+G, not at B. G = max(2 ms, 10% of 20 ms) = 2 ms, so
    // the hard deadline is 22 ms; the meter is the thread clock, so this is a
    // CPU measurement and not a wall-clock one.
    let cpu_ms = out.cpu_ns as f64 / 1e6;
    let hard_ms = budget.hard_ns() as f64 / 1e6;
    assert_eq!(hard_ms, 22.0);
    assert!(
        cpu_ms >= hard_ms,
        "killed after {cpu_ms:.2} ms, before the {hard_ms:.0} ms hard deadline — the grace \
         window was cut short"
    );
    assert!(
        cpu_ms < hard_ms + 8.0,
        "killed after {cpu_ms:.2} ms against a {hard_ms:.0} ms hard deadline"
    );
    // §6.4: the overage is recorded, and it is the CPU past B — grace included.
    assert!(
        out.overage_ns >= budget.grace_ns(),
        "a dispatch that ran the whole grace window must be recorded at least G of overage; \
         recorded {} ns against G = {} ns",
        out.overage_ns,
        budget.grace_ns()
    );
    eprintln!(
        "HARD KILL: B=20 ms G={:.0} ms => B+G={hard_ms:.0} ms; recorded {:.3} ms CPU \
         ({:.3} ms overage), {:.3} ms wall, overshoot past B+G {:.3} ms",
        budget.grace_ns() as f64 / 1e6,
        cpu_ms,
        out.overage_ns as f64 / 1e6,
        out.wall_ns as f64 / 1e6,
        cpu_ms - hard_ms
    );

    // §2.5.8, checked rather than asserted in prose: the guest was inside the
    // dispatch, and the code after the loop never ran. There is no unwinding
    // to run it — which is exactly why §5.4 can say "the host reclaims all of
    // the dispatch's handles on kill; invariant 2 *is* the cleanup story".
    let read = |m: &[u8], off: usize| u64::from_le_bytes(m[off..off + 8].try_into().unwrap());
    let (entered, cleaned_up) = {
        let m = i.memory();
        (read(m, 3000), read(m, 3008))
    };
    assert_eq!(entered, 1, "the guest must actually have been running");
    assert_eq!(
        cleaned_up, 0,
        "§2.5.8: no `finally`-equivalent may run after a budget kill"
    );

    // §6.4: the instance is a TOMBSTONE. Not "discarded by convention" —
    // every host-side operation on it fails, and a call into the guest is a
    // hard assertion failure. Kill-then-call is checked in its own
    // conformance test below; here it is enough that the state is set and the
    // two host-side operations that could launder it refuse.
    assert!(i.is_poisoned());
    assert!(i.tombstone_reason().is_some());
    assert!(!i.is_parked());
    assert!(
        i.reset().is_err(),
        "a hard-killed instance must not be laundered back into the pool"
    );
    assert!(
        i.capture_mutable_state().is_err(),
        "§6.4: deltas are never captured from a tombstone"
    );
}

// ---------------------------------------------------------------------------
// §6.4 — the three CONFORMANCE TESTS, named in the spec
//
//   1. kill-then-call MUST observe refusal;
//   2. a dispatch that yields during grace MUST be observed clean;
//   3. a genuine spin MUST tombstone at exactly B+G.
//
// They are written against hand-built `.wat` guests rather than the Rust SDK
// for the reason the fixture module gives: a guest-SDK bug must never be
// mistakable for an engine bug. The SDK's own cooperation is proved
// separately, end-to-end through a real cell, in
// `crates/wisp-host/tests/two_phase_kill.rs`.
// ---------------------------------------------------------------------------

/// **Conformance 1: kill-then-call MUST observe refusal.**
///
/// This is the test Appendix C F2 asked for. The finding was that wasmtime
/// does *not* poison a store after a budget trap — a killed instance answered
/// a later exported call correctly, globals intact — so "the instance is
/// discarded" is an invariant the platform has to enforce rather than one the
/// engine provides.
///
/// The refusal is a panic, which is what "hard assertion failure" means, so
/// the test has to catch it. That is the point: an `Err` could be ignored by
/// a caller, and a caller ignoring it is exactly the bug.
#[test]
fn conformance_kill_then_call_is_refused() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
  (global $spins (export "g_spins") (mut i64) (i64.const 0))
  (func $spin (loop $l (global.set $spins (i64.add (global.get $spins) (i64.const 1))) (br $l)))"#,
        "(call $spin) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let budget = CpuBudget {
        cpu_ms: 5,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    };

    let out = i.dispatch(&http_dispatch(1), budget);
    assert_eq!(out.completion, Completion::BudgetKill);
    assert!(i.is_poisoned(), "a hard kill must tombstone");

    // The call that F2 watched succeed. It must now be refused.
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        i.dispatch(&http_dispatch(2), budget)
    }));
    let payload = refused.expect_err(
        "a tombstoned instance answered a second dispatch — this is exactly Appendix C F2, and \
         §6.4 makes it a hard assertion failure",
    );
    let msg = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .unwrap_or_default()
        });
    assert!(msg.contains("TOMBSTONED"), "unhelpful refusal: {msg}");

    // Every other door is shut too, and these refuse rather than panic
    // because they are host-side operations on memory, not calls into a guest.
    assert!(i.reset().is_err());
    assert!(
        i.capture_mutable_state().is_err(),
        "deltas are never captured from a tombstone"
    );
    assert!(!i.is_capture_eligible());
    assert!(!i.is_parked());
}

/// **Conformance 2: a dispatch that yields during grace MUST be observed
/// clean (no tombstone).**
///
/// The fixture is the negative control's twin: the identical spin loop, plus
/// a `budget_remaining_ns` check every 4096 iterations. When the check goes
/// non-positive it stores a marker — the `finally` §2.5.8 says a *hard* kill
/// never runs — and returns `done`.
///
/// Everything the spec promises a clean kill is asserted here: the completion
/// is `CleanKill`, the instance is NOT tombstoned, `reset` works, the slot is
/// reusable for a subsequent dispatch, and the overage is recorded.
#[test]
fn conformance_a_dispatch_that_yields_during_grace_is_clean() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("budget_remaining_ns", "", "(result i64)"),
        r#"
  ;; The cooperative loop: a slab of pure arithmetic, then one read of the
  ;; host-published remaining budget, repeat. §4.6's runtime layer does the
  ;; same thing at its allocation slow path, GC entry and task switch.
  ;;
  ;; The slab size is the interesting parameter and it is what the third
  ;; conformance test is really about: cooperation is only cooperation if the
  ;; gap between checks is comfortably inside G. 1024 xorshift rounds is a few
  ;; microseconds against a 2 ms window. An earlier draft of this fixture
  ;; checked every 4096 turns of a loop whose body was itself a host call,
  ;; which put the checks ~1.6 ms apart and made the "clean" kill land past
  ;; B+G about half the time — a real bug in miniature, and exactly the one a
  ;; runtime layer with a coarse safepoint interval would ship.
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
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let budget = CpuBudget {
        cpu_ms: 20,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    };

    let out = i.dispatch(&http_dispatch(1), budget);

    assert!(
        out.trap.is_none(),
        "a clean kill is a return, not a trap: {:?}",
        out.trap
    );
    assert_eq!(out.completion, Completion::CleanKill);
    assert_eq!(out.kill, Some(KillPhase::Clean));
    assert!(
        out.completion.instance_survives(),
        "§6.4: the slot is reusable"
    );
    assert!(!out.completion.tombstones());

    // §2.5.8's difference, made visible: on a clean kill the code after the
    // loop DOES run. The hard-kill test above asserts the same address stays
    // zero, and the two fixtures differ only in the budget check.
    let read = |m: &[u8], off: usize| u64::from_le_bytes(m[off..off + 8].try_into().unwrap());
    let (entered, cleaned_up) = {
        let m = i.memory();
        (read(m, 3000), read(m, 3008))
    };
    assert_eq!(entered, 1);
    assert_eq!(cleaned_up, 1, "§6.4: on a clean kill `finally` runs");

    // The overage is real and is recorded: it ran past B, inside G, and stopped.
    let cpu_ms = out.cpu_ns as f64 / 1e6;
    assert!(
        cpu_ms >= 20.0,
        "the dispatch must actually have crossed B, not merely returned early"
    );
    assert!(
        out.cpu_ns < budget.hard_ns(),
        "a clean kill must land INSIDE the grace window: {cpu_ms:.3} ms against a {:.0} ms hard \
         deadline",
        budget.hard_ns() as f64 / 1e6
    );
    assert!(out.overage_ns > 0, "§6.4: the overage CPU is recorded");
    eprintln!(
        "CLEAN KILL: B=20 ms G={:.0} ms; returned after {:.3} ms CPU ({:.3} ms overage, \
         {:.1}% of G used)",
        budget.grace_ns() as f64 / 1e6,
        cpu_ms,
        out.overage_ns as f64 / 1e6,
        100.0 * out.overage_ns as f64 / budget.grace_ns() as f64
    );

    // Not a tombstone: it can be reset and reused, which is the whole
    // economic point of the grace window.
    assert!(!i.is_poisoned());
    assert!(i.tombstone_reason().is_none());
    i.reset()
        .expect("§6.4: the slot is reusable after a clean kill");
    let again = i.dispatch(
        &http_dispatch(2),
        CpuBudget {
            cpu_ms: 20,
            wall_ms_cap: 30_000,
            spent_ns: 0,
        },
    );
    assert_eq!(
        again.completion,
        Completion::CleanKill,
        "the reused slot must run the guest again, not refuse it"
    );
}

/// **Conformance 3: a genuine spin MUST tombstone at exactly B+G.**
///
/// "Exactly" is bounded by the mechanism, not by wishes: epochs tick every
/// 200 µs and the check is at a back-edge, so the achievable statement is
/// "at B+G, within one epoch tick plus scheduling noise". This measures the
/// distribution over 30 runs and pins both sides — the kill must not come
/// early (that would mean grace was not really offered) and must not come
/// late (that would mean the deadline is soft).
///
/// It also runs the pinned-bit case, because §4.6's *unsafe-to-trap* bit is
/// the one mechanism that could turn the hard deadline into a negotiation.
#[test]
fn conformance_a_genuine_spin_tombstones_at_exactly_b_plus_g() {
    let e = test_engine();
    let spin = "(func $spin (loop $l (br $l)))";
    let plain = guest(4, "", spin, "(call $spin) (i64.const 0)", DONE);
    // The same spin, with §4.6's bit pinned before it starts. A pinned bit
    // buys exactly G, nothing more.
    let pinned = guest(
        4,
        &import("unsafe_to_trap", "(param i32)", ""),
        spin,
        "(call $unsafe_to_trap (i32.const 1)) (call $spin) (i64.const 0)",
        DONE,
    );

    for (label, wasm) in [("spin", plain), ("spin+pinned-bit", pinned)] {
        let t = e.load_template(&wasm).unwrap();
        let budget = CpuBudget {
            cpu_ms: 10,
            wall_ms_cap: 30_000,
            spent_ns: 0,
        };
        let (b, g, hard) = (budget.budget_ns(), budget.grace_ns(), budget.hard_ns());
        assert_eq!((b, g, hard), (10_000_000, 2_000_000, 12_000_000));

        let mut deltas = Vec::new();
        let mut deferrals = 0u64;
        for n in 0..30u64 {
            let mut i = e
                .activate(&t, Box::new(TestBindings::new(Recorder::default())))
                .unwrap();
            let out = i.dispatch(&http_dispatch(n), budget);

            assert_eq!(
                out.completion,
                Completion::BudgetKill,
                "[{label}] must hard-kill"
            );
            assert_eq!(out.kill, Some(KillPhase::Hard));
            assert_eq!(out.trap.as_ref().unwrap().kind, TrapKind::BudgetExceeded);
            assert!(i.is_poisoned(), "[{label}] §6.4: a hard kill tombstones");
            assert!(
                !i.is_capture_eligible(),
                "[{label}] no delta from a tombstone"
            );
            assert!(
                out.cpu_ns >= hard,
                "[{label}] killed at {} ns, BEFORE the B+G deadline of {hard} ns — the grace \
                 window was not really offered",
                out.cpu_ns
            );
            deferrals += out.grace_deferrals;
            deltas.push(out.cpu_ns - hard);
        }
        deltas.sort_unstable();
        eprintln!(
            "[{label}] B+G = {:.0} ms; overshoot past it p50={:.0} us p90={:.0} us max={:.0} us \
             (grace deferrals: {deferrals})",
            hard as f64 / 1e6,
            pct(&deltas, 0.50) as f64 / 1000.0,
            pct(&deltas, 0.90) as f64 / 1000.0,
            *deltas.last().unwrap() as f64 / 1000.0
        );
        // One epoch tick is 200 µs; allow a generous multiple for scheduling
        // noise on a shared machine but stay far below G itself, so "exactly
        // B+G" remains a claim with teeth.
        assert!(
            pct(&deltas, 0.90) < g / 2,
            "[{label}] the hard deadline is soft: p90 overshoot {} ns against G = {g} ns",
            pct(&deltas, 0.90)
        );
        if label.contains("pinned") {
            assert!(
                deferrals > 0,
                "the pinned-bit fixture must actually have caused grace entry to defer, or it is \
                 not testing what it claims"
            );
        }
    }
}

#[test]
fn budget_kill_latency_is_bounded_by_the_epoch_tick() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        "(func $spin (loop $l (br $l)))",
        "(call $spin) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();

    let mut over = Vec::new();
    for n in 0..40u64 {
        let mut i = e
            .activate(&t, Box::new(TestBindings::new(Recorder::default())))
            .unwrap();
        let b = CpuBudget {
            cpu_ms: 5,
            wall_ms_cap: 30_000,
            spent_ns: 0,
        };
        let out = i.dispatch(&http_dispatch(n), b);
        assert_eq!(out.trap.as_ref().unwrap().kind, TrapKind::BudgetExceeded);
        // Overshoot is measured past B+G, which is where the kill is now due.
        over.push(out.cpu_ns.saturating_sub(b.hard_ns()));
    }
    over.sort_unstable();
    eprintln!(
        "BUDGET KILL OVERSHOOT over a 5 ms budget (n=40): p50={:.0} us p90={:.0} us p99={:.0} us max={:.0} us",
        pct(&over, 0.50) as f64 / 1000.0,
        pct(&over, 0.90) as f64 / 1000.0,
        pct(&over, 0.99) as f64 / 1000.0,
        *over.last().unwrap() as f64 / 1000.0
    );
    assert!(
        pct(&over, 0.99) < 5_000_000,
        "overshoot must stay well inside the budget itself"
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
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    // Put a recognisable message where `abort` will read it.
    i.memory()[0..5].copy_from_slice(b"boom!");

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    let trap = out.trap.unwrap();
    assert_eq!(trap.kind, TrapKind::GuestPanic, "{trap:?}");
    assert!(trap.message.contains("boom!"), "{}", trap.message);
    assert!(i.is_poisoned());
}

#[test]
fn a_bounds_violation_is_classified_as_a_sandbox_trap() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        "",
        // Well past a 4-page memory.
        "(drop (i64.load (i32.const 1000000000))) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.trap.as_ref().unwrap().kind,
        TrapKind::Sandbox,
        "{:?}",
        out.trap
    );
    assert_eq!(out.completion, Completion::Trapped);
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
    let t = e.load_template(&wat::parse_str(src).unwrap()).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.trap.as_ref().unwrap().kind,
        TrapKind::OutOfMemory,
        "{:?}",
        out.trap
    );
}

// ---------------------------------------------------------------------------
// §11.2 / G2 / C3 — the meter
// ---------------------------------------------------------------------------

#[test]
fn metered_cpu_is_stable_and_excludes_time_spent_parked() {
    let e = test_engine();
    // Park on the first dispatch after a fixed amount of work; complete on the
    // wake after the same amount again.
    let wasm = guest(
        4,
        "",
        "",
        "(drop (call $burn (i32.const 400000))) (i64.const 1)",
        "(drop (call $burn (i32.const 400000))) (i64.const 0)",
    );
    let t = e.load_template(&wasm).unwrap();

    // Stability: the same work must bill the same CPU across runs.
    let mut samples = Vec::new();
    for n in 0..25u64 {
        let mut i = e
            .activate(&t, Box::new(TestBindings::new(Recorder::default())))
            .unwrap();
        let out = i.dispatch(&http_dispatch(n), CpuBudget::http());
        assert_eq!(out.completion, Completion::Parked);
        samples.push(out.cpu_ns);
    }
    samples.sort_unstable();
    let (p50, p99, min) = (pct(&samples, 0.5), pct(&samples, 0.99), samples[0]);
    eprintln!(
        "METER: 400k-iteration dispatch recorded p50={:.3} ms p99={:.3} ms min={:.3} ms (n=25, spread {:.1}%)",
        p50 as f64 / 1e6,
        p99 as f64 / 1e6,
        min as f64 / 1e6,
        100.0 * (p99 - min) as f64 / p50 as f64
    );
    assert!(p50 > 0, "the meter must resolve a millisecond of work");
    assert!(
        (p99 as f64) < (p50 as f64) * 2.0,
        "recorded CPU for identical work must not vary 2x: p50={p50} p99={p99}"
    );

    // Park time is not recorded. The instance parks, sits for 250 ms holding no
    // thread, then is woken; the sum of the two metered intervals must be
    // nowhere near the wall time that elapsed.
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let t0 = std::time::Instant::now();
    let a = i.dispatch(&http_dispatch(99), CpuBudget::http());
    assert_eq!(a.completion, Completion::Parked);
    assert!(
        i.is_parked(),
        "§6.6: an instance that returned `pending` is Parked"
    );

    std::thread::sleep(std::time::Duration::from_millis(250));

    let b = i.wake(1, &ResultRecord::Ok(vec![1, 2, 3]), CpuBudget::http());
    assert_eq!(b.completion, Completion::Done);
    let total_wall = t0.elapsed().as_nanos() as u64;
    let recorded = a.cpu_ns + b.cpu_ns;

    eprintln!(
        "PARK: recorded {:.3} ms CPU across a {:.1} ms wall-clock lifetime ({:.1}% of wall)",
        recorded as f64 / 1e6,
        total_wall as f64 / 1e6,
        100.0 * recorded as f64 / total_wall as f64
    );
    assert!(
        recorded < 250_000_000,
        "§11.2: park time is never metered, but {recorded} ns was recorded over a 250 ms park"
    );
    assert!(total_wall > 250_000_000);
}

// ---------------------------------------------------------------------------
// §8.4 / §8.5 — the frozen clock is a security control
// ---------------------------------------------------------------------------

#[test]
fn the_coarse_clock_does_not_advance_within_a_dispatch_slice() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("clock_now_coarse", "", "(result i64)"),
        "",
        // Read, burn, read, burn, read. If the clock were a timer, these would
        // differ and the guest would have the measurement primitive §8.4 exists
        // to remove.
        r#"
    (i64.store (i32.const 0) (call $clock_now_coarse))
    (drop (call $burn (i32.const 200000)))
    (i64.store (i32.const 8) (call $clock_now_coarse))
    (drop (call $burn (i32.const 200000)))
    (i64.store (i32.const 16) (call $clock_now_coarse))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .unwrap();

    i.dispatch(&http_dispatch(1), CpuBudget::http());
    let read = |m: &[u8], off: usize| u64::from_le_bytes(m[off..off + 8].try_into().unwrap());
    let (a, b, c) = {
        let m = i.memory();
        (read(m, 0), read(m, 8), read(m, 16))
    };
    assert_eq!(a, b, "the clock advanced mid-slice");
    assert_eq!(b, c, "the clock advanced mid-slice");

    // The host clock genuinely moves — otherwise the assertion above is
    // vacuous — and the *next* slice sees the new value.
    i.dispatch(&http_dispatch(2), CpuBudget::http());
    let (d, e2, f) = {
        let m = i.memory();
        (read(m, 0), read(m, 8), read(m, 16))
    };
    assert_eq!(d, e2);
    assert_eq!(e2, f);
    assert!(d > a, "a new slice must see a fresh clock ({a} -> {d})");
    assert!(rec.clock_reads.load(Ordering::Relaxed) >= 2);

    eprintln!(
        "FROZEN CLOCK: slice 1 read {a} three times across ~400k iterations; slice 2 read {d}. \
         Host clock was sampled {} times for 6 guest reads.",
        rec.clock_reads.load(Ordering::Relaxed)
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
    ;; A synchronous import is not a host-call boundary for clock purposes:
    ;; otherwise `log_emit` in a loop would be a stopwatch (§8.4).
    (call $log_emit (i32.const 1) (i32.const 2048) (i32.const 4))
    (i64.store (i32.const 8) (call $clock_now_coarse))
    ;; A future-returning import is one: control genuinely left the guest.
    (drop (call $kv_get (i32.const 2048) (i32.const 4)))
    (i64.store (i32.const 16) (call $clock_now_coarse))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .unwrap();
    i.dispatch(&http_dispatch(1), CpuBudget::http());

    let read = |m: &[u8], off: usize| u64::from_le_bytes(m[off..off + 8].try_into().unwrap());
    let m = i.memory();
    let (a, b, c) = (read(m, 0), read(m, 8), read(m, 16));
    assert_eq!(
        a, b,
        "a synchronous import must not advance the frozen clock"
    );
    assert!(
        c > b,
        "a future-returning import is a host-call boundary ({b} -> {c})"
    );

    let calls = rec.calls.lock().clone();
    assert!(calls.iter().any(|c| c.starts_with("log_emit")), "{calls:?}");
    assert!(calls.iter().any(|c| c.starts_with("kv_get")), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("clock_now_coarse")),
        "the frozen clock is engine-owned and must never reach a binding: {calls:?}"
    );
}

// ---------------------------------------------------------------------------
// §5.5 — buffers, in both directions
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
    ;; guest -> host: the engine copies these 12 bytes out before the call.
    (i64.store (i32.const 2048) (i64.const 1234567890))
    (i64.store (i32.const 2056) (i64.const 0))
    (drop (call $kv_get (i32.const 2048) (i32.const 12)))
    ;; host -> guest: the host fills a host-side buffer, the engine copies in.
    (call $entropy_fill (i32.const 3072) (i32.const 32))
    (i64.const 0)"#,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .unwrap();
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);

    let calls = rec.calls.lock().clone();
    assert!(calls.contains(&"kv_get(12 bytes)".to_string()), "{calls:?}");
    // The out-buffer import reaches the host with an *empty* payload; the space
    // it is to fill is a host-side buffer, never a pointer into guest memory.
    assert!(
        calls.contains(&"entropy_fill(0 bytes)".to_string()),
        "{calls:?}"
    );
    assert_eq!(&i.memory()[3072..3104], &[0xABu8; 32]);
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
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert!(out.trap.is_some());
    assert!(
        out.trap.as_ref().unwrap().message.contains("outside"),
        "{:?}",
        out.trap
    );
}

#[test]
fn the_default_host_bindings_deny_out_buffer_fills() {
    // A zero-filled default would return a constant instead of host entropy.
    // The default binding denies the fill.
    let e = test_engine();
    let wasm = guest(
        4,
        &import("entropy_fill", "(param i32 i32)", ""),
        "",
        "(call $entropy_fill (i32.const 3072) (i32.const 32)) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut b = TestBindings::new(Recorder::default());
    b.allow_fill = false;
    let mut i = e.activate(&t, Box::new(b)).unwrap();
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        &i.memory()[3072..3104],
        &[0u8; 32],
        "denied fills must write nothing"
    );
}

// ---------------------------------------------------------------------------
// §6.6 / §4.5 / C4 — "checked, not assumed"
// ---------------------------------------------------------------------------

#[test]
fn park_state_is_checked_against_the_real_wasm_stack_depth() {
    let e = test_engine();
    let wasm = guest(4, "", "", PENDING, DONE);
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();

    // Never dispatched: an empty stack, but no futures — not Parked.
    assert!(!i.is_parked(), "a fresh instance is not Parked (§6.5)");
    assert_eq!(i.max_wasm_depth_observed(), 0, "no wasm frame has ever run");

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(out.status, PollStatus::Pending);
    assert!(i.is_parked());

    // The depth counter is wasmtime's own view of the native stack, not our
    // bookkeeping around the call: a frame really was pushed and really was
    // popped. Without the first assertion, "depth == 0" would be vacuous.
    assert_eq!(
        i.max_wasm_depth_observed(),
        1,
        "a wasm frame must have been observed on the stack during the dispatch"
    );

    // §5.4's single-caller rule, observed from inside: the host never re-enters
    // a running instance, so the depth never exceeds one.
    i.wake(1, &ResultRecord::Ok(vec![]), CpuBudget::http());
    assert_eq!(i.max_wasm_depth_observed(), 1);
    assert!(
        !i.is_parked(),
        "an instance that returned `done` is not Parked"
    );

    // A trapped instance is never a capture candidate however empty its stack.
    let wasm = guest(4, "", "", "(unreachable)", DONE);
    let t2 = e.load_template(&wasm).unwrap();
    let mut j = e
        .activate(&t2, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    j.dispatch(&http_dispatch(1), CpuBudget::http());
    assert!(!j.is_parked());
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
    let t = e.load_template(&wasm).unwrap();
    assert!(t.mutable_global_names().count() >= 4);

    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    for n in 0..3 {
        i.dispatch(&http_dispatch(n), CpuBudget::http());
    }
    let img = i.capture_mutable_state().unwrap();
    assert_eq!(img.memory_pages, 4);

    // A fresh activation is at the template's sealed values; restoring the
    // captured image must put it exactly where the first one was.
    let mut j = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let fresh = j.capture_mutable_state().unwrap();
    assert_ne!(format!("{:?}", fresh.globals), format!("{:?}", img.globals));
    j.restore_mutable_state(&img).unwrap();
    assert_eq!(
        format!("{:?}", j.capture_mutable_state().unwrap().globals),
        format!("{:?}", img.globals)
    );
}

// ---------------------------------------------------------------------------
// §4.4 — the sealed init sandbox
// ---------------------------------------------------------------------------

#[test]
fn the_init_sandbox_traps_on_a_forbidden_import_and_names_it() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}\n{}",
        import("clock_now_coarse", "", "(result i64)"),
        import("log_emit", "(param i32 i32 i32)", ""),
        import("init_unit_begin", "(param i32)", ""),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "wisp_initialize")
    (call $init_unit_begin (i32.const 4))
    ;; Pure so far. This one is not: reading the clock makes the cctor impure
    ;; and it must be deferred to lazy per-instance execution (§4.4).
    (i64.store (i32.const 0) (call $clock_now_coarse)))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();

    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::init_sandbox(rec.clone())))
        .unwrap();
    let out = i.run_initialize(CpuBudget {
        cpu_ms: 10_000,
        wall_ms_cap: 10_000,
        spent_ns: 0,
    });

    let trap = out
        .trap
        .as_ref()
        .expect("an impure initializer must trap in the sealed sandbox");
    assert_eq!(trap.kind, TrapKind::CapabilityViolation, "{trap:?}");
    assert_eq!(trap.kind.tenant_code(), "capability_violation");
    assert!(
        trap.message.contains("clock_now_coarse"),
        "must name the import: {}",
        trap.message
    );
    assert_eq!(i.last_capability_violation(), Some("clock_now_coarse"));

    // The init-visible import ran; the forbidden one never reached a binding.
    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("init_unit_begin")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("clock_now_coarse")),
        "{calls:?}"
    );

    eprintln!("INIT SANDBOX: unit 4 deferred, reason = {}", trap.message);
}

#[test]
fn the_init_sandbox_permits_exactly_the_init_visible_set() {
    let e = test_engine();
    // §4.4: the escrowed hash seed is the one *externally sourced* value; the
    // config blob is tenant-authored deploy input, which is why pre-running a
    // config parse bakes no shared secret.
    let imports = format!(
        "{}\n{}\n{}",
        import("init_hash_seed", "(param i32 i32)", ""),
        import("init_config_get", "(param i32 i32)", "(result i32)"),
        import("init_unit_deferred", "(param i32)", "(result i32)"),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "wisp_initialize")
    (if (i32.eqz (call $init_unit_deferred (i32.const 1)))
      (then
        (call $init_hash_seed (i32.const 512) (i32.const 16))
        ;; The zero-length probe: `(ptr, cap) -> i32` answers with the blob's
        ;; FULL length, so a guest can size its buffer before asking for bytes.
        (i32.store (i32.const 600) (call $init_config_get (i32.const 0) (i32.const 0)))
        (i32.store (i32.const 604) (call $init_config_get (i32.const 1024) (i32.const 8))))))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut b = TestBindings::init_sandbox(Recorder::default());
    b.full_length = Some(9_000);
    let mut i = e.activate(&t, Box::new(b)).unwrap();
    let out = i.run_initialize(CpuBudget {
        cpu_ms: 10_000,
        wall_ms_cap: 10_000,
        spent_ns: 0,
    });
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);

    let m = i.memory();
    // `TestBindings::call_fill` varies its byte per call — a fake CSPRNG that
    // answered a constant could not tell a per-call draw from a cached one —
    // so each fill is checked for being uniform and non-zero rather than for
    // one particular value. What matters here is the *extent* copied, which
    // is what the (ptr, cap) contract is about.
    let seed = &m[512..528];
    assert!(
        seed.iter().all(|b| *b == seed[0]) && seed[0] != 0,
        "the escrowed seed must be delivered"
    );
    assert_eq!(
        u32::from_le_bytes(m[600..604].try_into().unwrap()),
        9_000,
        "a zero-capacity probe must report the full length, not what was copied"
    );
    assert_eq!(u32::from_le_bytes(m[604..608].try_into().unwrap()), 9_000);
    let capped = &m[1024..1032];
    assert!(
        capped.iter().all(|b| *b == capped[0]) && capped[0] != 0,
        "the capped read must fill exactly `cap` bytes"
    );
    assert_eq!(m[1032], 0, "and must not write one byte past `cap`");
    assert_ne!(
        seed[0], capped[0],
        "two host fills in one dispatch must not be the same draw"
    );

    // Cross-check against the contract: every import the sandbox admits is
    // marked init_visible, and nothing in a binding group is.
    let visible: Vec<&str> = wisp_abi::imports::init_visible().map(|d| d.name).collect();
    assert!(visible.contains(&"init_hash_seed"));
    assert!(visible.contains(&"init_config_get"));
    assert!(!visible.contains(&"clock_now_coarse"));
}

#[test]
fn outside_the_init_sandbox_every_declared_import_is_reachable() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("clock_now_coarse", "", "(result i64)"),
        r#"(func (export "wisp_initialize") (i64.store (i32.const 0) (call $clock_now_coarse)))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let out = i.run_initialize(CpuBudget::http());
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);
    assert_eq!(i.last_capability_violation(), None);
}

// ---------------------------------------------------------------------------
// §6.2 bookkeeping
// ---------------------------------------------------------------------------

#[test]
fn fixed_bookkeeping_is_within_the_64_kb_target() {
    let e = test_engine();
    let t = e.load_template(&echo_guest()).unwrap();
    let i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    let b = i.bookkeeping_bytes();
    eprintln!(
        "BOOKKEEPING: {b} B/instance ({} B committed by the pooling allocator + host structs)",
        e.bookkeeping_bytes_per_instance()
    );
    assert!(
        b <= 64 * 1024,
        "§6.2 targets <= 64 KB of fixed bookkeeping, got {b}"
    );
}

// ---------------------------------------------------------------------------
// §5.5 / §6.6 — the pre-capture GC hint
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
    let t = e.load_template(&wasm).unwrap();
    assert!(t.has_gc_hint());

    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert!(i.is_parked());

    let out = i.gc_hint(255);
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);

    let m = i.memory();
    assert_eq!(u32::from_le_bytes(m[2048..2052].try_into().unwrap()), 255);
    assert_eq!(u32::from_le_bytes(m[2052..2056].try_into().unwrap()), 1);

    // The whole point of the hint is that capture happens next.
    assert!(
        i.is_parked(),
        "§6.6 calls gc-hint on a Parked instance immediately before capture; \
         the hint must not clear park state"
    );
    i.capture_mutable_state()
        .expect("a hinted, parked instance must still be capturable");
}

#[test]
fn gc_hint_is_optional() {
    // §5.5: "a guest that ignores the hint is correct but stores a larger delta
    // (and pays for it)". Not exporting it at all is the same choice, earlier.
    let e = test_engine();
    let t = e.load_template(&guest(4, "", "", PENDING, DONE)).unwrap();
    assert!(!t.has_gc_hint());
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(i.gc_hint(255).completion, Completion::Done);
    assert!(i.is_parked());
}

// ---------------------------------------------------------------------------
// §6.4 — the wall-clock backstop is a guard, not a meter
// ---------------------------------------------------------------------------

#[test]
fn the_wall_clock_backstop_reports_its_distinct_cap() {
    let e = test_engine();
    let wasm = guest(
        4,
        "",
        "(func $spin (loop $l (br $l)))",
        "(call $spin) (i64.const 0)",
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();

    // A CPU budget it will never reach, and a wall cap it will.
    let out = i.dispatch(
        &http_dispatch(1),
        CpuBudget {
            cpu_ms: 600_000,
            wall_ms_cap: 40,
            spent_ns: 0,
        },
    );
    let trap = out.trap.as_ref().expect("the backstop must trip");
    assert_eq!(trap.kind, TrapKind::BudgetExceeded, "{trap:?}");
    assert!(
        trap.message.contains("wall-clock backstop"),
        "the message must distinguish the backstop from the CPU meter, since \
         §11.2 says wall time is excluded from execution CPU accounting: {}",
        trap.message
    );
    assert!(
        out.wall_ns >= 40_000_000,
        "tripped after {} ns of wall",
        out.wall_ns
    );
    assert!(
        out.cpu_ns < 600_000 * 1_000_000,
        "the CPU budget was never reached; the kill came from the wall guard"
    );
    eprintln!(
        "WALL BACKSTOP: 40 ms cap tripped at {:.1} ms wall / {:.1} ms CPU",
        out.wall_ns as f64 / 1e6,
        out.cpu_ns as f64 / 1e6
    );
}

// ---------------------------------------------------------------------------
// §4.3 V2 / §8.2 — the features that make blocking inexpressible
// ---------------------------------------------------------------------------

/// V2 is the rule that makes "no blocking" structural rather than advisory: with
/// threads and shared memory off, `memory.atomic.wait*`/`notify` fail bytecode
/// validation and a guest cannot express a wait at all (§4.5).
///
/// This is asserted empirically because arguing it from cargo features is not
/// safe: cargo unifies features across a workspace, `wisp-control` depends on
/// wasmtime with default features, and `WasmFeatures::THREADS` defaults to
/// **true**. The engine therefore disables the proposals explicitly, and this
/// test is what would catch it if that ever stopped working.
#[test]
fn threads_shared_memory_and_atomics_are_rejected_by_the_engine() {
    let e = test_engine();

    let cases: [(&str, &str); 4] = [
        (
            "shared memory",
            r#"(module (memory (export "memory") 1 1 shared)
                       (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
                       (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "atomic wait",
            r#"(module (memory (export "memory") 1 1 shared)
                       (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "wisp_handle") (param i32 i32) (result i64)
                         (drop (memory.atomic.wait32 (i32.const 0) (i32.const 0) (i64.const -1)))
                         (i64.const 0))
                       (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "atomic rmw on unshared memory",
            r#"(module (memory (export "memory") 1)
                       (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "wisp_handle") (param i32 i32) (result i64)
                         (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
                         (i64.const 0))
                       (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "two linear memories (V3)",
            r#"(module (memory (export "memory") 1) (memory 1)
                       (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
                       (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
    ];

    for (what, src) in cases {
        // Some of these do not even assemble once the feature is off, which is
        // itself the rule holding; the ones that do assemble must be rejected
        // by `load_template`.
        match wat::parse_str(src) {
            Err(e) => eprintln!("  V2 `{what}`: rejected at assembly ({e})"),
            Ok(wasm) => {
                let err = e
                    .load_template(&wasm)
                    .err()
                    .unwrap_or_else(|| panic!("V2/V3: `{what}` must not load"));
                eprintln!(
                    "  V2 `{what}`: rejected -> {}",
                    format!("{err:#}").lines().next().unwrap()
                );
            }
        }
    }

    // Relaxed SIMD is off too (§4.3 V2: "deterministic SIMD subset only"), while
    // the deterministic subset still works.
    let relaxed = r#"(module (memory (export "memory") 1)
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "wisp_handle") (param i32 i32) (result i64)
          (drop (i32x4.relaxed_trunc_f32x4_s (v128.const i32x4 0 0 0 0)))
          (i64.const 0))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    match wat::parse_str(relaxed) {
        Ok(wasm) => {
            let err = e
                .load_template(&wasm)
                .err()
                .expect("V2: relaxed SIMD must be rejected");
            eprintln!(
                "  V2 `relaxed SIMD`: rejected -> {}",
                format!("{err:#}").lines().next().unwrap()
            );
        }
        Err(e) => eprintln!("  V2 `relaxed SIMD`: rejected at assembly ({e})"),
    }
    let deterministic = r#"(module (memory (export "memory") 1)
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "wisp_handle") (param i32 i32) (result i64)
          (drop (i32x4.add (v128.const i32x4 1 2 3 4) (v128.const i32x4 5 6 7 8)))
          (i64.const 0))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    e.load_template(&wat::parse_str(deterministic).unwrap())
        .expect("the deterministic SIMD subset must still load");
}

// ---------------------------------------------------------------------------
// §4.3 — the WASI PAL adapter, at RUNTIME
//
// `wisp-control` proves the adapter validates and pre-initializes. These
// prove the other half: that a module carrying `wasi:*` imports now
// *instantiates in a cell*, and that the two channel properties survive the
// trip through the data plane's linker. Both properties fail silently if
// broken — the module still runs, every other test still passes, and a
// channel §8 closed is quietly open again — which is exactly why they are
// asserted here and not assumed from the deploy-time tests.
// ---------------------------------------------------------------------------

/// A guest that imports `wasi:*` as well as `wisp`, with the `cabi_realloc`
/// export the canonical ABI needs to return a `list<u8>`.
fn pal_guest(handle: &str) -> Vec<u8> {
    let imports = format!(
        r#"
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $wasi_now (result i64)))
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $wasi_random (param i64 i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $wasi_exit (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $wasi_env (param i32)))
"#
    );
    let funcs = r#"
  ;; The canonical ABI's allocator. A bump allocator over a fixed arena is
  ;; enough: the adapter calls it once per `get-random-bytes`.
  (global $cabi (mut i32) (i32.const 40000))
  (func (export "cabi_realloc")
        (param $old i32) (param $oldsz i32) (param $align i32) (param $newsz i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $cabi))
    (global.set $cabi
      (i32.and (i32.add (i32.add (local.get $p) (local.get $newsz)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))
"#;
    guest(4, &imports, funcs, handle, DONE)
}

/// §4.3: a module declaring `wasi:*` imports **instantiates**. Before the
/// adapter reached the data plane's linker it could pass validation and
/// pre-initialization and then fail at activation, which is the worst place
/// for a capability decision to surface.
#[test]
fn a_module_with_wasi_imports_instantiates_through_the_pal_adapter() {
    let e = test_engine();
    let wasm = pal_guest("(i64.const 0)");
    let t = e
        .load_template(&wasm)
        .expect("a whitelisted wasi: surface must instantiate");

    assert!(
        t.uses_pal(),
        "the adapter should have been composed for this module"
    );
    let composed: Vec<String> = t.pal_composed().iter().map(|d| d.full_name()).collect();
    assert_eq!(composed.len(), 4, "composed: {composed:?}");

    // §4.3's audit trail: a capability that came in through a WASI door must
    // be nameable as the `wisp:platform` capability it resolves to, or "no new
    // channels" is a promise nobody can check.
    let caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    assert!(caps.contains(&"clock_now_coarse"), "caps: {caps:?}");
    assert!(caps.contains(&"entropy_fill"), "caps: {caps:?}");
    assert_eq!(
        caps.len(),
        2,
        "the adapter drew on more than the two declared capabilities: {caps:?}"
    );

    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done
    );
}

/// §8.4/§8.5, through the runtime linker: `monotonic-clock::now` is
/// **quantized to the coarse clock and frozen intra-slice**.
///
/// The negative control is the whole test. A real nanosecond monotonic clock
/// — which is what `wasmtime-wasi` would hand you, and the obvious thing to
/// reach for — passes "the guest can read the time" and fails both assertions
/// below. §8.4 denies guests a fine timer because it is the measurement
/// primitive most Spectre-class attacks need.
#[test]
fn the_pal_clock_is_quantized_and_frozen_within_a_slice() {
    let e = test_engine();
    // Two reads in one dispatch, stored side by side.
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (call $wasi_now))
    (i64.store (i32.const 3008) (call $wasi_now))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).unwrap();
    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(rec.clone())))
        .unwrap();

    let read = |i: &mut wisp_engine::Instance, off: usize| -> i64 {
        let m = i.memory();
        i64::from_le_bytes(m[off..off + 8].try_into().unwrap())
    };

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done
    );
    let (a, b) = (read(&mut i, 3000), read(&mut i, 3008));
    assert_eq!(
        a, b,
        "§8.5: the clock is FROZEN within a slice — two reads cannot differ"
    );
    assert_eq!(
        a % 1_000_000,
        0,
        "§8.4/§4.3: the delegated clock must be quantized to the coarse clock's 1 ms, so no \
         sub-millisecond bit exists to leak; got {a} ns"
    );

    // Across slices it may advance — and must still be quantized. A clock
    // that never moved would pass the two assertions above while being
    // useless, so this is the control on the control.
    assert_eq!(
        i.dispatch(&http_dispatch(2), CpuBudget::http()).completion,
        Completion::Done
    );
    let c = read(&mut i, 3000);
    assert!(
        c > a,
        "the coarse clock must still advance between slices: {a} then {c}"
    );
    assert_eq!(
        c % 1_000_000,
        0,
        "quantization must survive the slice boundary"
    );
}

/// §8.5, through the runtime linker: `random::get-random-bytes` reaches the
/// host CSPRNG **per call and caches nothing**.
///
/// Entropy captured into a
/// template, or memoized anywhere on the delegation path, would be shared by
/// every clone of that template — and would look completely fine, because
/// random-looking bytes arrive either way.
#[test]
fn the_pal_entropy_delegation_draws_per_call_and_never_caches() {
    let e = test_engine();
    // Two 16-byte draws in one dispatch, into two return areas.
    let wasm = pal_guest(
        r#"
    (call $wasi_random (i64.const 16) (i32.const 3000))
    (call $wasi_random (i64.const 16) (i32.const 3016))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut b = TestBindings::new(Recorder::default());
    b.allow_fill = true;
    let mut i = e.activate(&t, Box::new(b)).unwrap();

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done
    );

    // Each return area holds (ptr, len) into guest memory.
    let (p1, l1, p2, l2) = {
        let m = i.memory();
        let rd = |o: usize| u32::from_le_bytes(m[o..o + 4].try_into().unwrap()) as usize;
        (rd(3000), rd(3004), rd(3016), rd(3020))
    };
    assert_eq!(
        (l1, l2),
        (16, 16),
        "the adapter must return the requested length"
    );
    assert_ne!(
        p1, p2,
        "each draw must allocate its own buffer through `cabi_realloc`"
    );

    let (first, second) = {
        let m = i.memory();
        (m[p1..p1 + 16].to_vec(), m[p2..p2 + 16].to_vec())
    };
    // `TestBindings::call_fill` writes an incrementing byte per call, so two
    // draws differ iff the host was really asked twice. A cached buffer —
    // anywhere on the path — makes these equal.
    assert_ne!(
        first, second,
        "§8.5/§4.4: two draws in one dispatch returned identical bytes, so something on the \
         delegation path cached the CSPRNG output; clones of this template would share an RNG \
         stream and nothing else in the suite would notice"
    );
}

/// §4.3: a `TrapOnCall` entry **traps when called**, and does not quietly
/// return zero.
///
/// This is the negative control for the whitelist itself. A silent
/// zero-returning stub really does succeed — the guest carries on, the
/// dispatch returns 200, and a denied capability has become a supported one
/// that happens to answer badly. §13's disposition for a trap (fail the
/// dispatch, discard the instance) is the honest answer for a declaration
/// that is toolchain residue.
#[test]
fn a_pal_trap_on_call_stub_traps_rather_than_answering() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_exit (i32.const 0))
    (i64.store (i32.const 3000) (i64.const 1))   ;; must never run
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "a denied capability must not answer"
    );
    let msg = out
        .trap
        .as_ref()
        .map(|t| t.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("wasi:cli/exit") && msg.contains("trap-on-call"),
        "the trap must name the import and say why: {msg}"
    );
    assert!(i.is_poisoned(), "§13: a trapped instance is discarded");
}

/// §2.5.4, through the runtime linker: `get-environment` answers the **empty
/// list**, which is the same fidelity as absent.
#[test]
fn the_pal_environment_delegation_answers_empty() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (i64.const -1))  ;; poison the return area
    (call $wasi_env (i32.const 3000))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .unwrap();
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done
    );

    let (ptr, len) = {
        let m = i.memory();
        let rd = |o: usize| u32::from_le_bytes(m[o..o + 4].try_into().unwrap());
        (rd(3000), rd(3004))
    };
    assert_eq!(
        (ptr, len),
        (0, 0),
        "§2.5.4: there is no ambient environment, so the list is empty"
    );
}
