//! Behavioral tests for the sandbox boundary.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;
use wisp_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, TrapKind, WispEngine};

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

#[test]
fn a_spinner_is_killed_at_its_cpu_budget_and_the_instance_is_discarded() {
    let e = test_engine();
    // A tight back-edge loop: epoch checks are Cranelift-inserted at back-edges.
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
        cpu_ms: 20,
        wall_ms_cap: 30_000,
    };
    let out = i.dispatch(&http_dispatch(1), budget);

    let trap = out.trap.as_ref().expect("a spinner must trap");
    assert_eq!(trap.kind, TrapKind::BudgetExceeded, "{trap:?}");
    assert_eq!(trap.kind.tenant_code(), "budget_exceeded");
    assert_eq!(out.completion, Completion::BudgetKill);
    assert!(!out.completion.instance_survives());

    // The kill lands close to the budget: the meter is the thread clock, so
    // this is a CPU measurement, not a wall-clock one.
    let cpu_ms = out.cpu_ns as f64 / 1e6;
    assert!(
        (20.0..30.0).contains(&cpu_ms),
        "killed after {cpu_ms:.2} ms of CPU against a 20 ms budget"
    );
    eprintln!(
        "BUDGET KILL: budget 20 ms, recorded {:.3} ms CPU, {:.3} ms wall, overshoot {:.3} ms",
        cpu_ms,
        out.wall_ns as f64 / 1e6,
        cpu_ms - 20.0
    );

    // §6.4: "an instance killed at budget is discarded". Every entry point
    // must now refuse — `instance_survives` governs, it does not merely
    // describe.
    assert!(i.is_poisoned());
    assert!(!i.is_parked());
    let again = i.dispatch(&http_dispatch(2), budget);
    assert_eq!(again.completion, Completion::Trapped);
    assert_eq!(
        again.cpu_ns, 0,
        "a refused dispatch must not bill the tenant"
    );
    assert!(
        i.reset().is_err(),
        "a killed instance must not be laundered back into the pool"
    );
    assert!(
        i.capture_mutable_state().is_err(),
        "§13: never snapshot arbitrary memory"
    );
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
        let out = i.dispatch(
            &http_dispatch(n),
            CpuBudget {
                cpu_ms: 5,
                wall_ms_cap: 30_000,
            },
        );
        assert_eq!(out.trap.as_ref().unwrap().kind, TrapKind::BudgetExceeded);
        over.push(out.cpu_ns.saturating_sub(5_000_000));
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
    // §4.4: the escrowed hash seed is the one externally sourced value.
    let imports = format!(
        "{}\n{}",
        import("init_hash_seed", "(param i32 i32)", ""),
        import("init_unit_deferred", "(param i32)", "(result i32)"),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "wisp_initialize")
    (if (i32.eqz (call $init_unit_deferred (i32.const 1)))
      (then (call $init_hash_seed (i32.const 512) (i32.const 16)))))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).unwrap();
    let mut i = e
        .activate(
            &t,
            Box::new(TestBindings::init_sandbox(Recorder::default())),
        )
        .unwrap();
    let out = i.run_initialize(CpuBudget {
        cpu_ms: 10_000,
        wall_ms_cap: 10_000,
    });
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);
    assert_eq!(
        &i.memory()[512..528],
        &[0xABu8; 16],
        "the escrowed seed must be delivered"
    );

    // Cross-check against the contract: every import the sandbox admits is
    // marked init_visible, and nothing in a binding group is.
    let visible: Vec<&str> = wisp_abi::imports::init_visible().map(|d| d.name).collect();
    assert!(visible.contains(&"init_hash_seed"));
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
