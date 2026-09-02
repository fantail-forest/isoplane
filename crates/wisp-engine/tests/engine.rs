//! Behavioural tests for the sandbox boundary.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, InstanceOps, KillPhase, TrapKind, WispEngine,
};

/// A small engine: the guard is not the default because every test process
/// would otherwise reserve tens of GiB of address space per engine it builds.
fn small_engine_config() -> EngineConfig {
    EngineConfig {
        max_instances: 64,
        max_memory_bytes: 4 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
    }
}

fn test_engine() -> WispEngine {
    WispEngine::new(small_engine_config()).expect("engine")
}

fn echo_guest() -> Vec<u8> {
    guest(4, "", "", DONE, DONE)
}

fn activate(e: &WispEngine, t: &wisp_engine::Template) -> wisp_engine::Instance {
    e.activate(t, Box::new(TestBindings::new(Recorder::default())))
        .expect("activation")
}

fn read_u64(m: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
}

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

fn budget(cpu_ms: u32) -> CpuBudget {
    CpuBudget {
        cpu_ms,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    }
}

const SPIN: &str = "(func $spin (loop $l (br $l)))";

// ---------------------------------------------------------------------------
// imports are the complete capability surface
// ---------------------------------------------------------------------------

#[test]
fn an_import_outside_the_table_fails_to_load() {
    let e = test_engine();

    let wasm = guest(
        4,
        r#"(import "wisp" "definitely_not_an_import" (func $x (result i64)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("an undeclared import must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("definitely_not_an_import"),
        "the error must name the offending import: {msg}"
    );

    let wasm = guest(
        4,
        r#"(import "wasi_snapshot_preview1" "fd_write" (func $x (param i32) (result i32)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("an import from an unknown module must be rejected");
    assert!(
        format!("{err:#}").contains("fd_write"),
        "the error must name the offending import: {err:#}"
    );

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
    let src = r#"
(module
  (memory (export "memory") 1)
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 1024))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0)))"#;
    let t = e
        .load_template(&wat::parse_str(src).expect("wat"))
        .expect("the module compiles");
    let err = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .expect_err("a module without wisp_wake is not deployable");
    assert!(
        format!("{err:#}").contains("wisp_wake"),
        "the error must name the missing export: {err:#}"
    );
}

#[test]
fn a_template_whose_start_function_calls_an_import_is_refused() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("init_unit_begin", "(param i32)", ""),
        r#"
  (func $start (call $init_unit_begin (i32.const 1)))
  (start $start)"#,
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("a template that reaches the host during instantiation must be refused");
    assert!(
        format!("{err:#}").contains("called a host import"),
        "the error must say the start function reached the host: {err:#}"
    );
}

#[test]
fn a_module_with_more_table_elements_than_the_pool_allows_is_refused() {
    let e = test_engine();
    let wasm = guest(4, "", "(table 131073 funcref)", DONE, DONE);
    e.load_template(&wasm)
        .expect_err("a table over the pool's element cap must not load");
    let wasm = guest(4, "", "(table 131072 funcref)", DONE, DONE);
    let t = e.load_template(&wasm).expect("a table at the cap loads");
    activate(&e, &t);
}

// ---------------------------------------------------------------------------
// engine construction
// ---------------------------------------------------------------------------

#[test]
fn an_epoch_tick_too_coarse_for_the_grace_floor_is_refused_at_construction() {
    let err = WispEngine::new(EngineConfig {
        epoch_tick_us: 5_000,
        ..small_engine_config()
    })
    .err()
    .expect("a 5 ms tick cannot deliver a 2 ms grace window and must be refused");
    assert!(
        format!("{err:#}").contains("grace"),
        "the refusal must say what the tick is too coarse for: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// activation and reset
// ---------------------------------------------------------------------------

#[test]
fn reset_restores_template_contents_byte_exactly() {
    let e = test_engine();
    let t = e.load_template(&echo_guest()).expect("template");
    let image = t.memory_image_arc().to_dense();

    let mut i = activate(&e, &t);
    assert_eq!(
        i.memory(),
        image.as_slice(),
        "a fresh activation must equal the template"
    );

    for off in [0usize, 4096, 65536, 30 * 4096, 60 * 4096] {
        i.memory()[off..off + 8].copy_from_slice(&[0xAB; 8]);
    }
    assert_ne!(
        i.memory(),
        image.as_slice(),
        "the writes must have changed memory"
    );

    i.reset().expect("reset");

    assert_eq!(
        i.memory(),
        image.as_slice(),
        "reset must restore template contents byte for byte"
    );
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
  (global $counter (export "g_counter") (mut i64) (i64.const 41))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let baseline = activate(&e, &t).capture_mutable_state().expect("capture");

    let mut a = activate(&e, &t);
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
    a.restore_mutable_state(&img).expect("restore");
    let mutated = a.capture_mutable_state().expect("capture");
    assert_ne!(
        format!("{:?}", mutated.globals),
        format!("{:?}", baseline.globals),
        "the restore must have changed the globals"
    );

    let mut b = activate(&e, &t);
    let fresh = b.capture_mutable_state().expect("capture");
    assert_eq!(
        format!("{:?}", fresh.globals),
        format!("{:?}", baseline.globals),
        "a fresh activation must see the template's sealed globals"
    );
    assert_eq!(
        b.memory(),
        t.memory_image_arc().to_dense(),
        "a fresh activation must see the template's memory"
    );
    assert!(
        fresh.table_mutations.is_empty(),
        "a fresh activation's table must match the template baseline"
    );

    a.reset().expect("reset");
    let after = a.capture_mutable_state().expect("capture");
    assert_eq!(
        format!("{:?}", after.globals),
        format!("{:?}", baseline.globals),
        "reset must re-seal globals from the template"
    );
}

#[test]
fn a_mapped_artifact_is_equivalent_to_the_compiled_template() {
    let e = test_engine();
    let dir = tempfile::tempdir().expect("tempdir");
    let wasm = echo_guest();
    let t = e.load_template(&wasm).expect("template");

    // SAFETY: a fresh directory this test owns, written only by this engine.
    let (a, compiled_a) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("first load");
    let (b, compiled_b) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("second load");
    assert!(
        compiled_a,
        "the first load has nothing to map and must compile"
    );
    assert!(
        !compiled_b,
        "the second load must map the artifact the first wrote"
    );

    for m in [&a, &b] {
        assert_eq!(m.hash, t.hash, "the mapped template is the same module");
        assert_eq!(
            m.abi_rev, t.abi_rev,
            "the mapped template has the same ABI rev"
        );
        assert_eq!(
            *m.memory_image_arc(),
            *t.memory_image_arc(),
            "the mapped template has the same memory image"
        );
        assert_eq!(
            m.memory_pages(),
            t.memory_pages(),
            "the mapped template has the same memory size"
        );
    }

    let mut i = activate(&e, &b);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "a mapped template dispatches like a compiled one"
    );

    let other = WispEngine::new(EngineConfig {
        spectre_mitigations: false,
        ..small_engine_config()
    })
    .expect("engine");
    let paths = e
        .precompile_to_files(&wasm, dir.path())
        .expect("precompile");
    let meta = std::fs::read(&paths.meta).expect("read meta");
    // SAFETY: the metadata was written by `e` just above.
    let err = unsafe { other.deserialize_file(&meta, &paths.cwasm) }
        .expect_err("a differently configured engine must refuse the artifact");
    assert!(
        format!("{err:#}").contains("engine"),
        "the refusal must say the engine does not match: {err:#}"
    );
}

#[test]
fn a_tampered_artifact_is_refused_before_it_is_mapped() {
    let e = test_engine();
    let dir = tempfile::tempdir().expect("tempdir");
    let wasm = echo_guest();
    let paths = e
        .precompile_to_files(&wasm, dir.path())
        .expect("precompile");

    let mut cwasm = std::fs::read(&paths.cwasm).expect("read cwasm");
    let mid = cwasm.len() / 2;
    cwasm[mid] ^= 0xFF;
    std::fs::write(&paths.cwasm, &cwasm).expect("write cwasm");

    // SAFETY: the directory is this test's own; the point is that the tamper
    // is caught before wasmtime sees the file.
    let err = unsafe { e.load_mapped(&wasm, dir.path()) }
        .expect_err("a .cwasm that does not match its metadata must be refused");
    assert!(
        format!("{err:#}").contains("does not match the hash"),
        "the refusal must say the hash failed: {err:#}"
    );
}

#[test]
fn deferred_init_units_travel_in_the_template() {
    let e = test_engine();
    let mut wasm = echo_guest();
    let mut payload = 3u32.to_le_bytes().to_vec();
    for id in [7u32, 11, 13] {
        payload.extend_from_slice(&id.to_le_bytes());
    }
    append_custom_section(&mut wasm, wisp_engine::DEFERRED_INIT_SECTION, &payload);

    let t = e.load_template(&wasm).expect("template");
    assert_eq!(
        t.deferred_init_units(),
        &[7, 11, 13],
        "the section names the deferred units"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    // SAFETY: a fresh directory this test owns.
    let (back, _) = unsafe { e.load_mapped(&wasm, dir.path()) }.expect("mapped load");
    assert_eq!(
        back.deferred_init_units(),
        &[7, 11, 13],
        "the deferred units survive the artifact cache"
    );
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
    wasm.push(0);
    leb(body.len() as u32, wasm);
    wasm.extend_from_slice(&body);
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
// the init sandbox
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
    (i64.store (i32.const 0) (call $clock_now_coarse)))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::init_sandbox(rec.clone())))
        .expect("activation");
    let out = i.run_initialize(budget(10_000));

    let trap = out
        .trap
        .as_ref()
        .expect("an impure initializer must trap in the init sandbox");
    assert_eq!(
        trap.kind,
        TrapKind::CapabilityViolation,
        "reading the clock at init is a capability violation: {trap:?}"
    );
    assert!(
        trap.message.contains("clock_now_coarse"),
        "the trap must name the import: {}",
        trap.message
    );
    assert_eq!(
        i.last_capability_violation(),
        Some("clock_now_coarse"),
        "the violation is recorded for the deferral loop"
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("init_unit_begin")),
        "the init-visible import ran: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("clock_now_coarse")),
        "the forbidden import must trap before reaching a binding: {calls:?}"
    );
}

#[test]
fn the_init_sandbox_permits_exactly_the_init_visible_set() {
    let e = test_engine();
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
        (i32.store (i32.const 600) (call $init_config_get (i32.const 0) (i32.const 0)))
        (i32.store (i32.const 604) (call $init_config_get (i32.const 1024) (i32.const 8))))))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut b = TestBindings::init_sandbox(Recorder::default());
    b.full_length = Some(9_000);
    let mut i = e.activate(&t, Box::new(b)).expect("activation");
    let out = i.run_initialize(budget(10_000));
    assert_eq!(
        out.completion,
        Completion::Done,
        "the init-visible imports are reachable: {:?}",
        out.trap
    );

    let m = i.memory();
    let seed = &m[512..528];
    assert!(
        seed.iter().all(|b| *b == seed[0]) && seed[0] != 0,
        "the hash seed must be delivered into the guest's buffer"
    );
    assert_eq!(
        read_u32(m, 600),
        9_000,
        "a zero-capacity probe must report the full length, not what was copied"
    );
    assert_eq!(
        read_u32(m, 604),
        9_000,
        "a capped read still reports the full length"
    );
    let capped = &m[1024..1032];
    assert!(
        capped.iter().all(|b| *b == capped[0]) && capped[0] != 0,
        "the capped read must fill exactly `cap` bytes"
    );
    assert_eq!(m[1032], 0, "the capped read must not write past `cap`");
    assert_ne!(
        seed[0], capped[0],
        "two host fills in one dispatch must be two draws"
    );

    let visible: Vec<&str> = wisp_abi::imports::init_visible().map(|d| d.name).collect();
    assert!(
        visible.contains(&"init_hash_seed") && visible.contains(&"init_config_get"),
        "the sandbox admits what the table marks init-visible: {visible:?}"
    );
    assert!(
        !visible.contains(&"clock_now_coarse"),
        "the clock is not init-visible: {visible:?}"
    );
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
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.run_initialize(CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Done,
        "outside the sandbox the clock is reachable: {:?}",
        out.trap
    );
    assert_eq!(
        i.last_capability_violation(),
        None,
        "no violation is recorded outside the sandbox"
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

// ---------------------------------------------------------------------------
// the features that make blocking inexpressible
// ---------------------------------------------------------------------------

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
            "two linear memories",
            r#"(module (memory (export "memory") 1) (memory 1)
                       (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
                       (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
    ];

    for (what, src) in cases {
        let wasm = wat::parse_str(src)
            .unwrap_or_else(|err| panic!("the `{what}` fixture must assemble: {err}"));
        assert!(
            e.load_template(&wasm).is_err(),
            "a module using {what} must not load"
        );
    }

    let relaxed = r#"(module (memory (export "memory") 1)
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "wisp_handle") (param i32 i32) (result i64)
          (drop (i32x4.relaxed_trunc_f32x4_s (v128.const i32x4 0 0 0 0)))
          (i64.const 0))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    let wasm = wat::parse_str(relaxed).expect("the relaxed SIMD fixture must assemble");
    assert!(
        e.load_template(&wasm).is_err(),
        "relaxed SIMD must be rejected; only the deterministic subset is allowed"
    );
    let deterministic = r#"(module (memory (export "memory") 1)
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "wisp_handle") (param i32 i32) (result i64)
          (drop (i32x4.add (v128.const i32x4 1 2 3 4) (v128.const i32x4 5 6 7 8)))
          (i64.const 0))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    e.load_template(&wat::parse_str(deterministic).expect("wat"))
        .expect("the deterministic SIMD subset must still load");
}

// ---------------------------------------------------------------------------
// the WASI PAL adapter at runtime
// ---------------------------------------------------------------------------

/// A guest that imports `wasi:*` as well as `wisp`, with the `cabi_realloc`
/// export the canonical ABI needs to return a `list<u8>`.
fn pal_guest(handle: &str) -> Vec<u8> {
    let imports = String::from(
        r#"
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $wasi_now (result i64)))
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $wasi_random (param i64 i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $wasi_exit (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $wasi_env (param i32)))
"#,
    );
    let funcs = r#"
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

#[test]
fn a_module_with_wasi_imports_instantiates_through_the_pal_adapter() {
    let e = test_engine();
    let wasm = pal_guest("(i64.const 0)");
    let t = e
        .load_template(&wasm)
        .expect("a whitelisted wasi: surface must instantiate");

    assert!(t.uses_pal(), "the adapter was composed for this module");
    let composed: Vec<String> = t.pal_composed().iter().map(|d| d.full_name()).collect();
    assert_eq!(
        composed.len(),
        4,
        "every declared wasi: import was composed: {composed:?}"
    );

    let mut caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    caps.sort_unstable();
    assert_eq!(
        caps,
        vec!["clock_now_coarse", "entropy_fill"],
        "the adapter drew on exactly the two declared capabilities"
    );

    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the module dispatches"
    );
}

/// A nanosecond monotonic clock would pass "the guest can read the time" and
/// fail both assertions; a fine timer is the measurement primitive
/// Spectre-class attacks need.
#[test]
fn the_pal_clock_is_quantized_and_frozen_within_a_slice() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (call $wasi_now))
    (i64.store (i32.const 3008) (call $wasi_now))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let read = |i: &mut wisp_engine::Instance, off: usize| -> i64 {
        let m = i.memory();
        i64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
    };

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );
    let (a, b) = (read(&mut i, 3000), read(&mut i, 3008));
    assert_eq!(
        a, b,
        "the clock is frozen within a slice, so two reads cannot differ"
    );
    assert_eq!(
        a % 1_000_000,
        0,
        "the delegated clock is quantized to the coarse clock's millisecond: {a} ns"
    );

    assert_eq!(
        i.dispatch(&http_dispatch(2), CpuBudget::http()).completion,
        Completion::Done,
        "the second dispatch completes"
    );
    let c = read(&mut i, 3000);
    assert!(
        c > a,
        "the coarse clock must advance between slices ({a} then {c}), or the freeze test is \
         vacuous"
    );
    assert_eq!(
        c % 1_000_000,
        0,
        "quantization must survive the slice boundary"
    );
}

/// Entropy captured into a template, or memoised anywhere on the delegation
/// path, would be shared by every clone and look fine, because random-looking
/// bytes arrive either way.
#[test]
fn the_pal_entropy_delegation_draws_per_call_and_never_caches() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_random (i64.const 16) (i32.const 3000))
    (call $wasi_random (i64.const 16) (i32.const 3016))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (p1, l1, p2, l2) = {
        let m = i.memory();
        (
            read_u32(m, 3000) as usize,
            read_u32(m, 3004),
            read_u32(m, 3016) as usize,
            read_u32(m, 3020),
        )
    };
    assert_eq!(
        (l1, l2),
        (16, 16),
        "the adapter returns the requested length"
    );
    assert_ne!(
        p1, p2,
        "each draw allocates its own buffer through `cabi_realloc`"
    );

    let (first, second) = {
        let m = i.memory();
        (m[p1..p1 + 16].to_vec(), m[p2..p2 + 16].to_vec())
    };
    assert_ne!(
        first, second,
        "two draws in one dispatch returned identical bytes, so something on the delegation \
         path cached the CSPRNG output"
    );
}

/// A stub that returned zero would let the guest carry on, turning a denied
/// capability into a supported one that answers badly.
#[test]
fn a_pal_trap_on_call_stub_traps_rather_than_answering() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_exit (i32.const 0))
    (i64.store (i32.const 3000) (i64.const 1))   ;; must never run
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "a denied capability must not answer"
    );
    let msg = out.trap.map(|t| t.message).unwrap_or_default();
    assert!(
        msg.contains("wasi:cli/exit") && msg.contains("trap-on-call"),
        "the trap must name the import and say why: {msg}"
    );
    assert!(
        i.reset().is_err(),
        "a trapped instance is discarded, not reset"
    );
}

#[test]
fn the_pal_environment_delegation_answers_empty() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (i64.const -1))  ;; poison the return area
    (call $wasi_env (i32.const 3000))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (ptr, len) = {
        let m = i.memory();
        (read_u32(m, 3000), read_u32(m, 3004))
    };
    assert_eq!(
        (ptr, len),
        (0, 0),
        "there is no ambient environment, so the list is empty"
    );
}
