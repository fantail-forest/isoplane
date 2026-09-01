//! **A parked instance costs zero CPU.**
//!
//! This is one test in one binary on purpose. It measures
//! `CLOCK_PROCESS_CPUTIME_ID` across the whole process, and `cargo` runs the
//! tests within a single binary in parallel — so sharing a binary with, say,
//! the budget-killed-spinner test would measure the spinner. `cargo test` runs
//! test *binaries* one at a time, which is the isolation this needs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};

use wisp_host::cell::{Cell, CellConfig, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
use wisp_host::{Activator, ModuleId, Resolver};

struct Fixed(Vec<std::net::SocketAddr>);
impl Resolver for Fixed {
    fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(self.0.clone())
    }
}

fn cell(cfg: CellConfig) -> Arc<Cell> {
    let svc = Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])),
            1024,
            1 << 30,
        )),
        fetcher: Arc::new(FakeFetcher::default()),
    });
    Cell::new(cfg, svc)
}

fn manifest(name: &str, lifecycle: HandlerLifecycle, limits: Limits) -> AppManifest {
    AppManifest {
        name: name.into(),
        handlers: HandlerConfig { lifecycle },
        bindings: Bindings::default(),
        limits: limits,
        init_visible_vars: vec![],
    }
}

fn http(cell: &Cell, module: &ModuleId, path: &str) -> DispatchRecord {
    cell.http_record(
        module,
        HttpRequest {
            method: "GET".into(),
            url: format!("http://app.example{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    )
}

fn wait_for(mut cond: impl FnMut() -> bool, within: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// A host timer long enough that it will not fire during the measurement.
/// Timers are host futures, not guest loops — which is the mechanism the whole
/// claim rests on.
const PARK_MS: u64 = 600_000;

#[test]
fn parked_instance_cpu_use_stays_within_the_test_budget() {
    const N: usize = 1_000;
    let seconds: u64 = std::env::var("WISP_PARK_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let cell = cell(CellConfig {
        exec_threads: 8,
        max_hot: 8,
        admission_queue: 4096,
        max_idle_per_module: N + 16,
        ..CellConfig::for_test("c3")
    });
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("parker", "v1"),
        // Park on a host timer far beyond the test window. Timers are host
        // futures, not guest loops — which is the whole mechanism.
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    ));
    let module = act.module().clone();
    cell.register_module(
        "acme",
        act,
        manifest("parker", HandlerLifecycle::Reuse, Limits::default()),
    )
    .unwrap();

    // Baseline first: what does an *empty* cell cost? Reactor threads, the
    // timer wheel and the idle sweeper all run whether or not anything is
    // parked, and the claim is about the *marginal* cost of an instance, not
    // about the fixed cost of having a cell at all. Measuring the difference
    // also makes the test robust on a loaded build machine, where absolute
    // CPU readings drift.
    let base0 = wisp_host::meters::read_process_cpu_ns();
    let base_wall = Instant::now();
    std::thread::sleep(Duration::from_secs(2));
    let baseline_ns_per_s = (wisp_host::meters::read_process_cpu_ns() - base0) as f64
        / base_wall.elapsed().as_secs_f64();

    let mut pending = Vec::with_capacity(N);
    for i in 0..N {
        let mut rec = http(&cell, &module, "/park");
        rec.dispatch_id = i as u64 + 1;
        pending.push(cell.submit(
            &module,
            Target::Handler {
                route: "/park".into(),
            },
            rec,
        ));
    }
    assert!(
        pending.iter().all(|p| p.is_some()),
        "the cell shed while admitting {N} dispatches"
    );

    assert!(
        wait_for(
            || cell.resident_count() >= N && cell.hot_count() == 0,
            Duration::from_secs(30)
        ),
        "only {} of {N} instances parked (hot={})",
        cell.resident_count(),
        cell.hot_count()
    );
    let parked = cell.metrics().parked;
    assert!(parked >= N as u64, "expected >= {N} parked, saw {parked}");

    // Now: 1,000 instances, each with an outstanding host future, doing
    // nothing. Measure what that costs.
    let cpu0 = wisp_host::meters::read_process_cpu_ns();
    let wall0 = Instant::now();
    std::thread::sleep(Duration::from_secs(seconds));
    let cpu_ns = wisp_host::meters::read_process_cpu_ns() - cpu0;
    let wall_ns = wall0.elapsed().as_nanos() as u64;

    let secs = wall_ns as f64 / 1e9;
    let cores_used = cpu_ns as f64 / wall_ns as f64;
    let marginal_ns_per_s = (cpu_ns as f64 / secs - baseline_ns_per_s).max(0.0);
    let per_instance_us_per_s = marginal_ns_per_s / 1e3 / N as f64;
    eprintln!(
        "parked: empty cell {:.1} ms/s; {N} parked for {secs:.1}s -> {:.1} ms total          ({cores_used:.4} cores), marginal {per_instance_us_per_s:.2} µs/instance/s",
        baseline_ns_per_s / 1e6,
        cpu_ns as f64 / 1e6,
    );

    // "≈0" made falsifiable, twice over.
    //
    // The marginal bound is the claim: a parked instance holds no thread and
    // no stack, so the only CPU it can cost is whatever the cell spends
    // noticing it exists. The two bounds below are loose ceilings — they are
    // there to fail when parking stops being free, not to describe how cheap
    // it is.
    assert!(
        per_instance_us_per_s < 15.0,
        "each parked instance costs {per_instance_us_per_s:.1} µs/s of cell CPU; parking is not free"
    );
    // And the absolute, so a regression in the fixed overhead is caught too.
    assert!(
        cores_used < 0.03,
        "1,000 parked instances used {cores_used:.4} cores, exceeding the test budget"
    );

    let meters = cell.meters(&module).unwrap();
    let cpu_ms = meters.cpu_ms();
    eprintln!(
        "parked: tenant recorded {cpu_ms:.3} CPU-ms for {N} dispatches + {seconds}s of parking"
    );
    assert!(
        cpu_ms < 200.0,
        "recorded {cpu_ms} CPU-ms; the park window must contribute nothing"
    );
    assert!(meters.observed_park_ms() == 0 || meters.cpu_ns() > 0);
    cell.shutdown();
}
