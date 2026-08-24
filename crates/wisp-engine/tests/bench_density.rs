//! **Claim C1** — isolate economics (spec.md G1, §6.2).
//!
//! > *≥ 5,000 resident instances per commodity server; marginal memory per
//! > instance = dirtied pages only.*
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

#[test]
fn marginal_instance_cost_is_dirty_pages() {
    let n = env_usize("WISP_DENSITY_N", 2_000);
    let dirty = env_usize("WISP_DENSITY_DIRTY_PAGES", 16) as u32;
    let guard = env_usize("WISP_DENSITY_GUARD_MIB", 2) as u64 * (1 << 20);

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
    assert_eq!(&t.memory_image()[0..4], b"WISP", "the template image must be non-zero");

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
    let base_shared = smaps_kb("Shared_Clean:");
    let base_private = smaps_kb("Private_Dirty:");

    let t0 = std::time::Instant::now();
    let mut live: Vec<Instance> = Vec::with_capacity(n);
    let mut marks = Vec::new();
    for i in 0..n {
        let mut inst = e
            .activate(&t, Box::new(TestBindings::new(Recorder::default())))
            .unwrap_or_else(|err| panic!("activation {i} failed: {err:#}"));
        let out = inst.dispatch(&http_dispatch(i as u64), CpuBudget::http());
        assert_eq!(out.completion, Completion::Done, "{:?}", out.trap);
        live.push(inst);
        if i + 1 == 1 || (i + 1) % 500 == 0 || i + 1 == n {
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
    let book = live[0].bookkeeping_bytes();
    let end_rss = rss_kb();
    let end_shared = smaps_kb("Shared_Clean:");
    let end_private = smaps_kb("Private_Dirty:");

    let growth_kb = end_rss.saturating_sub(base_rss);
    let per_inst_kb = growth_kb as f64 / n as f64;
    let dirty_kb = dirty as f64 * 4.0;
    let naive_mib = (MEM_PAGES as usize * 65536 / (1 << 20)) * n;

    eprintln!("\n=== C1: instance density (spec G1, §6.2) ===");
    eprintln!(
        "template: {} MiB linear memory, {} MiB pre-initialized image, {} instances, \
         {dirty} pages dirtied each ({dirty_kb:.0} KiB)",
        MEM_PAGES as usize * 65536 >> 20,
        big::IMAGE_BYTES >> 20,
        n
    );
    eprintln!("baseline RSS (engine + module + pool reservation, 0 live): {base_rss} kB");
    for (i, r) in &marks {
        eprintln!(
            "  live={i:<6} RSS={r:<10} kB  (+{} kB, {:.1} kB/instance marginal)",
            r.saturating_sub(base_rss),
            r.saturating_sub(base_rss) as f64 / *i as f64
        );
    }
    eprintln!(
        "  Private_Dirty {base_private} -> {end_private} kB (+{:.1} kB/instance)",
        (end_private.saturating_sub(base_private)) as f64 / n as f64
    );
    eprintln!("  Shared_Clean  {base_shared} -> {end_shared} kB");
    eprintln!(
        "  engine-reported: dirty_page_count={dirty_reported} ({} kB), bookkeeping={book} B",
        dirty_reported * 4
    );
    eprintln!(
        "  MEASURED MARGINAL COST: {per_inst_kb:.1} kB/instance vs {dirty_kb:.0} kB dirtied \
         + {:.1} kB bookkeeping = {:.1} kB accounted",
        book as f64 / 1024.0,
        dirty_kb + book as f64 / 1024.0
    );
    eprintln!(
        "  naive cost with no page sharing would be {naive_mib} MiB; measured growth is {} MiB \
         ({:.0}x less)",
        growth_kb / 1024,
        naive_mib as f64 / (growth_kb as f64 / 1024.0).max(1.0)
    );
    eprintln!(
        "  activate + first dispatch: {elapsed:?} total => {:.1} us/instance",
        elapsed.as_secs_f64() * 1e6 / n as f64
    );
    let budget_gib = 64.0;
    eprintln!(
        "  EXTRAPOLATION: at {per_inst_kb:.1} kB/instance, {budget_gib:.0} GiB of instance \
         memory holds ~{:.0} instances of this template",
        budget_gib * 1024.0 * 1024.0 / per_inst_kb
    );

    // The guest dirties `dirty` pages; the engine's own `wisp_alloc` handoff of
    // the dispatch record dirties one or two more in the scratch region.
    assert!(
        (dirty as usize..dirty as usize + 4).contains(&dirty_reported),
        "engine reported {dirty_reported} dirty pages, expected ~{dirty}"
    );

    // The claim, as an assertion: marginal cost is the dirty set plus fixed
    // bookkeeping, not the template size. Allow 2x headroom over the accounted
    // figure for page-table and allocator overhead.
    let accounted_kb = dirty_kb + book as f64 / 1024.0;
    assert!(
        per_inst_kb < accounted_kb * 2.0,
        "marginal cost {per_inst_kb:.1} kB/instance is more than 2x the {accounted_kb:.1} kB \
         accounted for by dirty pages + bookkeeping — page sharing is not working"
    );
    assert!(
        per_inst_kb < (MEM_PAGES as f64 * 64.0) / 4.0,
        "marginal cost is within 4x of copying the whole template"
    );

    drop(live);
}
