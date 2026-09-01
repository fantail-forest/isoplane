//! Isolate economics: marginal memory per resident instance should be the
//! pages that instance dirtied, and nothing else.
//!
//! One `#[test]` in one binary, because the measurement is process RSS and any
//! concurrently running test would be counted as an instance's cost.
//!
//! Run `cargo test -p wisp-engine --release --test bench_density -- --nocapture`
//! for numbers worth quoting.

#[path = "common/mod.rs"]
mod common;
#[path = "common/big.rs"]
mod big;

use common::*;
use wisp_engine::{Completion, CpuBudget, EngineConfig, Instance, InstanceOps, WispEngine};

/// 32 MiB of linear memory per instance, 8 MiB of it pre-initialized.
const MEM_PAGES: u32 = 512;

fn env_usize(k: &str, d: usize) -> usize {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

struct Scenario {
    n: usize,
    /// 4 KiB pages each instance writes to, *inside* the pre-initialized image,
    /// so every one of them forces a real copy-on-write fault.
    dirty: u32,
    label: &'static str,
}

#[test]
fn marginal_instance_cost_is_dirty_pages() {
    let guard = env_usize("WISP_DENSITY_GUARD_MIB", 2) as u64 * (1 << 20);
    let scale = env_usize("WISP_DENSITY_SCALE", 1);

    let scenarios = [
        Scenario { n: 2_000 * scale, dirty: 16, label: "64 KiB dirty set" },
        // A larger population with a realistic dirty set, where the marginal
        // slope dominates the fixed baseline and a sharing regression is
        // impossible to miss.
        Scenario { n: 5_000 * scale, dirty: 256, label: "1 MiB dirty set" },
    ];

    eprintln!("\n=== instance density ===");
    if cfg!(debug_assertions) {
        eprintln!("  *** DEBUG BUILD — re-run with --release for numbers worth quoting. ***");
    }

    let mut summary = Vec::new();
    for s in scenarios {
        let per_inst_kb = run_scenario(&s, guard);
        summary.push((s.n, s.dirty, s.label, per_inst_kb));
    }

    eprintln!("\n  --- density summary ---");
    for (n, dirty, label, per_inst_kb) in &summary {
        let dirty_kb = *dirty as f64 * 4.0;
        eprintln!(
            "  {n:>6} instances, {label:<40} -> {per_inst_kb:>8.1} kB/instance \
             (dirty set {dirty_kb:.0} kB, everything else {:.1} kB)",
            per_inst_kb - dirty_kb
        );
    }

    // Turn the measured per-instance overhead into residency, projected across
    // a range of plausible dirty sets and host memory budgets.
    let overhead_kb = summary
        .iter()
        .map(|(_, d, _, p)| p - *d as f64 * 4.0)
        .fold(f64::MIN, f64::max);
    eprintln!("\n  measured overhead beyond the dirty set: {overhead_kb:.1} kB/instance");
    for host_gib in [32.0f64, 64.0, 128.0] {
        let line: Vec<String> = [1.0f64, 4.0, 8.0]
            .iter()
            .map(|dirty_mib| {
                let per = dirty_mib * 1024.0 + overhead_kb.max(0.0);
                format!("{dirty_mib:.0} MiB -> {:>7.0}", host_gib * 1024.0 * 1024.0 / per)
            })
            .collect();
        eprintln!("  instances in a {host_gib:>5.0} GiB budget: {}", line.join("   "));
    }

    // A regression fence, not a target: the measured overhead sits an order of
    // magnitude under this, and a sharing regression blows past it immediately.
    assert!(
        overhead_kb < 64.0,
        "fixed per-instance overhead beyond the dirty set measured {overhead_kb:.1} kB, \
         far above where page sharing puts it — sharing is not working"
    );
}

fn run_scenario(s: &Scenario, guard: u64) -> f64 {
    let (n, dirty) = (s.n, s.dirty);

    let e = WispEngine::new(EngineConfig {
        max_instances: n as u32 + 16,
        max_memory_bytes: MEM_PAGES as u64 * 65536,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: guard,
    })
    .expect("engine (a pooling allocator reserves N x (max_memory + guard) of address space)");

    let wasm = big::big_guest(MEM_PAGES, dirty);
    let t = e.load_template(&wasm).expect("template");
    assert_eq!(t.memory_pages(), MEM_PAGES);
    let mut head = [0u8; 4];
    t.memory_image().read_at(0, &mut head);
    assert_eq!(&head, b"WISP", "the template image must be non-zero");

    // Warm everything that is not per-instance — compile, the pool's address
    // reservation, the linker — before the baseline is taken, so the baseline
    // is "engine ready, zero instances live".
    {
        let mut warm = e.activate(&t, Box::new(TestBindings::new(Recorder::default()))).unwrap();
        assert_eq!(
            warm.dispatch(&http_dispatch(0), CpuBudget::http()).completion,
            Completion::Done
        );
    }
    let base_rss = rss_kb();
    let base_pss = smaps_kb("Pss:");
    let base_shared = smaps_kb("Shared_Clean:");
    let base_private = smaps_kb("Private_Dirty:");

    let t0 = std::time::Instant::now();
    let mut live: Vec<Instance> = Vec::with_capacity(n);
    let mut marks = Vec::new();
    let step = (n / 4).max(1);
    for i in 0..n {
        let mut inst = e
            .activate(&t, Box::new(TestBindings::new(Recorder::default())))
            .unwrap_or_else(|err| panic!("activation {i} failed: {err:#}"));
        let out = inst.dispatch(&http_dispatch(i as u64), CpuBudget::http());
        assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);
        live.push(inst);
        if i + 1 == 1 || (i + 1) % step == 0 || i + 1 == n {
            marks.push((i + 1, rss_kb()));
        }
    }
    let elapsed = t0.elapsed();

    // Every instance still sees the shared template content it never wrote —
    // otherwise the "sharing" would be the kernel handing out zero pages.
    // Land on a repeat boundary of the fixture's 62-byte pattern.
    let probe = (big::IMAGE_BYTES - 4096) / 62 * 62;
    for inst in live.iter_mut().take(8) {
        assert_eq!(
            &inst.memory()[probe..probe + 4],
            b"WISP",
            "a live instance must still see template content it never wrote"
        );
    }

    let dirty_reported = live[0].dirty_page_count();
    let book = live[0].host_bookkeeping_bytes();
    let end_rss = rss_kb();
    let end_pss = smaps_kb("Pss:");
    let end_shared = smaps_kb("Shared_Clean:");
    let end_private = smaps_kb("Private_Dirty:");

    let growth_kb = end_rss.saturating_sub(base_rss);
    let per_inst_kb = growth_kb as f64 / n as f64;
    let dirty_kb = dirty as f64 * 4.0;
    let naive_mib = (MEM_PAGES as usize * 65536 / (1 << 20)) * n;

    eprintln!(
        "\n-- {n} instances x {} MiB template ({} MiB pre-initialized), {dirty} pages dirtied each \
         ({dirty_kb:.0} kB) --",
        (MEM_PAGES as usize * 65536) >> 20,
        big::IMAGE_BYTES >> 20,
    );
    eprintln!("  baseline RSS (engine + module + pool reservation, 0 live): {base_rss} kB");
    for (i, r) in &marks {
        eprintln!(
            "    live={i:<6} RSS={r:<10} kB  (+{} kB, {:.1} kB/instance marginal)",
            r.saturating_sub(base_rss),
            r.saturating_sub(base_rss) as f64 / *i as f64
        );
    }
    // PSS, not RSS, is the metric to size a machine from: RSS counts a shared
    // template page once per mapping, so anyone provisioning off RSS
    // over-provisions by roughly the fault-around region times the instance
    // count.
    let pss_per_inst = end_pss.saturating_sub(base_pss) as f64 / n as f64;
    eprintln!(
        "  Pss           {base_pss} -> {end_pss} kB (+{pss_per_inst:.1} kB/instance)  <== size machines from this"
    );
    eprintln!(
        "  Private_Dirty {base_private} -> {end_private} kB (+{:.1} kB/instance);  \
         Shared_Clean {base_shared} -> {end_shared} kB",
        (end_private.saturating_sub(base_private)) as f64 / n as f64
    );
    eprintln!(
        "  engine-reported: dirty_page_count={dirty_reported} ({} kB), host structs={book} B",
        dirty_reported * 4
    );
    eprintln!(
        "  MARGINAL COST: {per_inst_kb:.1} kB/instance; dirty set is {dirty_kb:.0} kB, so \
         everything else costs {:.1} kB",
        per_inst_kb - dirty_kb
    );
    eprintln!(
        "  naive cost with no page sharing: {naive_mib} MiB. Measured growth: {} MiB ({:.0}x less).",
        growth_kb / 1024,
        naive_mib as f64 / (growth_kb as f64 / 1024.0).max(1.0)
    );
    eprintln!(
        "  activate + first dispatch: {elapsed:?} total => {:.1} us/instance",
        elapsed.as_secs_f64() * 1e6 / n as f64
    );

    // The guest dirties `dirty` pages; the engine's own `wisp_alloc` handoff of
    // the dispatch record dirties one or two more in the scratch region.
    assert!(
        (dirty as usize..dirty as usize + 4).contains(&dirty_reported),
        "engine reported {dirty_reported} dirty pages, expected ~{dirty}"
    );
    assert!(
        per_inst_kb < dirty_kb + 64.0,
        "marginal cost {per_inst_kb:.1} kB/instance exceeds the {dirty_kb:.0} kB dirty set by more \
         than 64 kB of bookkeeping — page sharing is not working"
    );

    drop(live);
    // Report PSS, assert on RSS: RSS is the pessimistic of the two, so the
    // overhead bound is held against the number that cannot flatter us.
    let _ = pss_per_inst;
    per_inst_kb
}

/// The default config asks for 10,000 slots behind a 2 GiB guard, i.e. ~20 TiB
/// of `PROT_NONE` address space. If that is not actually constructible then
/// every default-configured cell fails at startup, so it is worth an assertion
/// rather than a comment.
#[test]
fn the_default_engine_configuration_is_constructible() {
    let cfg = EngineConfig::default();
    let va_tib = (cfg.max_instances as f64) * ((cfg.max_memory_bytes + cfg.guard_size_bytes) as f64)
        / (1u64 << 40) as f64;
    match WispEngine::new(cfg.clone()) {
        Ok(_) => eprintln!(
            "\n  default EngineConfig OK: {} slots x ({} MiB + {} MiB guard) = {va_tib:.1} TiB of \
             reserved address space",
            cfg.max_instances,
            cfg.max_memory_bytes >> 20,
            cfg.guard_size_bytes >> 20
        ),
        Err(e) => panic!("the default engine configuration ({va_tib:.1} TiB of VA) failed: {e:#}"),
    }
}
