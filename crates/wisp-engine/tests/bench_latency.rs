//! **Claim C2** — activation p50 < 1 ms from a local template (spec.md G1),
//! plus the reset cost §6.2 says makes `handler_lifecycle = "fresh"` a policy
//! choice rather than an economic one (§2.6).
//!
//! One `#[test]`, because timing percentiles measured against a machine that is
//! simultaneously running other tests are not percentiles of anything.
//!
//! `cargo test -p wisp-engine --release --test bench_latency -- --nocapture`.

#[path = "common/mod.rs"]
mod common;
#[path = "common/big.rs"]
mod big;

use std::time::Instant;

use common::*;
use wisp_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, WispEngine};

const MEM_PAGES: u32 = 512;

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn report(name: &str, mut ns: Vec<u64>) -> (f64, f64) {
    ns.sort_unstable();
    let us = |v: u64| v as f64 / 1000.0;
    let (p50, p99) = (us(pct(&ns, 0.50)), us(pct(&ns, 0.99)));
    eprintln!(
        "  {name:<38} p50={p50:>9.2} us  p90={:>9.2} us  p99={p99:>9.2} us  \
         p99.9={:>9.2} us  max={:>9.2} us  (n={})",
        us(pct(&ns, 0.90)),
        us(pct(&ns, 0.999)),
        us(*ns.last().unwrap()),
        ns.len()
    );
    (p50, p99)
}

#[test]
fn activation_and_reset_latency() {
    let iters = env_usize("WISP_LATENCY_ITERS", 5_000);
    let e = WispEngine::new(EngineConfig {
        max_instances: 256,
        max_memory_bytes: MEM_PAGES as u64 * 65536,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: env_usize("WISP_LATENCY_GUARD_MIB", 2) as u64 * (1 << 20),
    })
    .unwrap();

    let t0 = Instant::now();
    let t = e.load_template(&big::big_guest(MEM_PAGES, 16)).unwrap();
    let compile = t0.elapsed();

    eprintln!("\n=== C2: activation latency from a local template (spec G1) ===");
    if cfg!(debug_assertions) {
        eprintln!(
            "  *** DEBUG BUILD — these numbers are not the claim. Re-run with --release. ***"
        );
    }
    eprintln!(
        "  template: {} MiB memory / {} MiB pre-initialized image; Cranelift compile took {compile:?}",
        MEM_PAGES as usize * 65536 >> 20,
        big::IMAGE_BYTES >> 20
    );

    // Warm the pool so we measure steady-state slot acquisition, which is what
    // a cell does, rather than first-touch of the pool's reservation.
    for _ in 0..200 {
        drop(e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap());
    }

    // 1. Cold activation: §6.2 steps 1-3. Map the template CoW, seal
    //    globals/tables, bind the import vtable.
    let mut act = Vec::with_capacity(iters);
    for _ in 0..iters {
        let b = Box::new(TestBindings::new(Recorder::default()));
        let s = Instant::now();
        let inst = e.activate(&t, b).unwrap();
        act.push(s.elapsed().as_nanos() as u64);
        drop(inst);
    }
    let (act_p50, act_p99) = report("activate", act);

    // 2. Activation *and* the first dispatch — what a cold request actually
    //    pays (§6.3).
    let mut first = Vec::with_capacity(iters);
    for i in 0..iters {
        let b = Box::new(TestBindings::new(Recorder::default()));
        let rec = http_dispatch(i as u64);
        let s = Instant::now();
        let mut inst = e.activate(&t, b).unwrap();
        let out = inst.dispatch(&rec, CpuBudget::http());
        first.push(s.elapsed().as_nanos() as u64);
        assert_eq!(out.completion, Completion::Done);
        drop(inst);
    }
    let (first_p50, first_p99) = report("activate + first dispatch", first);

    // 3. Reset (§6.2). "madvise(MADV_DONTNEED) restores template contents in
    //    microseconds" — the number that decides whether §2.6's `fresh` mode is
    //    a hygiene policy or an economic constraint.
    let mut reset_clean = Vec::with_capacity(iters);
    {
        let mut inst = e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap();
        for _ in 0..iters {
            let s = Instant::now();
            inst.reset().unwrap();
            reset_clean.push(s.elapsed().as_nanos() as u64);
        }
    }
    let (reset_p50, _) = report("reset (nothing dirtied)", reset_clean);

    // Warm-instance dispatch, to decompose the cold-request number above.
    let mut warm_dispatch = Vec::with_capacity(iters);
    {
        let mut inst = e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap();
        for i in 0..iters {
            let rec = http_dispatch(i as u64);
            let s = Instant::now();
            inst.dispatch(&rec, CpuBudget::http());
            warm_dispatch.push(s.elapsed().as_nanos() as u64);
        }
    }
    report("dispatch on a warm instance", warm_dispatch);

    let mut reset_dirty = Vec::with_capacity(iters);
    {
        let mut inst = e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap();

        // Check the premise ONCE, outside the timed loop. `dirty_page_count`
        // is a full page-diff, so it reads every page of the 32 MiB memory and
        // faults the whole mapping resident; doing that inside the loop would
        // measure the instrumentation rather than the reset. (Noted as a real
        // property of the diff-based implementation, not just a test artifact.)
        inst.dispatch(&http_dispatch(0), CpuBudget::http());
        assert!(inst.dirty_page_count() >= 16);
        inst.reset().unwrap();
        assert_eq!(inst.dirty_page_count(), 0);
        inst.reset().unwrap();

        for i in 0..iters {
            // 16 pages dirtied by the guest, plus the record handoff.
            inst.dispatch(&http_dispatch(i as u64), CpuBudget::http());
            let s = Instant::now();
            inst.reset().unwrap();
            reset_dirty.push(s.elapsed().as_nanos() as u64);
        }
    }
    let (reset_dirty_p50, reset_dirty_p99) = report("reset (64 KiB dirtied first)", reset_dirty);

    eprintln!(
        "\n  => C2 target is activation p50 < 1000 us, p99 < 5000 us. Measured p50={act_p50:.2} us, \
         p99={act_p99:.2} us."
    );
    eprintln!(
        "  => cold request (activate + dispatch) p50={first_p50:.2} us p99={first_p99:.2} us."
    );
    eprintln!(
        "  => §2.6 `fresh` costs {reset_dirty_p50:.2} us p50 / {reset_dirty_p99:.2} us p99 per \
         dispatch on top of the request. That is the price of \"no request can observe residue \
         of any prior request\"."
    );
    let _ = reset_p50;

    if !cfg!(debug_assertions) {
        assert!(act_p50 < 1000.0, "G1: activation p50 must be under 1 ms, measured {act_p50:.2} us");
        assert!(act_p99 < 5000.0, "G1: activation p99 must be under 5 ms, measured {act_p99:.2} us");
        assert!(
            reset_dirty_p50 < 1000.0,
            "§6.2 says reset restores template contents in microseconds, measured {reset_dirty_p50:.2} us"
        );
    }
}

/// The watchdog is what delivers budget kills (§6.4), and it is also a thread
/// that could quietly put a floor under an idle cell's CPU — which would
/// contradict G2 before a single guest ran. It must tick when work is Hot and
/// stop when it is not.
#[test]
fn the_epoch_watchdog_is_idle_when_nothing_is_hot() {
    let e = WispEngine::new(EngineConfig {
        max_instances: 8,
        max_memory_bytes: 4 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
    })
    .unwrap();

    let idle_before = e.epoch_ticks();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let idle_ticks = e.epoch_ticks() - idle_before;

    let wasm = guest(4, "", "", "(drop (call $burn (i32.const 2000000))) (i64.const 0)", DONE);
    let t = e.load_template(&wasm).unwrap();
    let mut i = e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap();

    let before = e.epoch_ticks();
    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);
    let hot_ticks = e.epoch_ticks() - before;
    let hot_ms = out.wall_ns as f64 / 1e6;

    eprintln!("\n=== §6.4 epoch watchdog ===");
    eprintln!("  idle 200 ms: {idle_ticks} ticks (a free-running 200 us ticker would be ~1000)");
    eprintln!(
        "  hot {hot_ms:.2} ms: {hot_ticks} ticks => achieved tick {:.1} us (target 200 us)",
        if hot_ticks > 0 { hot_ms * 1000.0 / hot_ticks as f64 } else { 0.0 }
    );

    assert!(idle_ticks <= 2, "the watchdog must park when no dispatch is Hot, saw {idle_ticks}");
    if !cfg!(debug_assertions) && hot_ms > 2.0 {
        assert!(hot_ticks > 0, "the watchdog must tick while a dispatch is Hot");
    }
}
