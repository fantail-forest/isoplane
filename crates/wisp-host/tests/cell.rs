use std::sync::Arc;
use std::time::{Duration, Instant};

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::result::{FutureError, ResultRecord};
use wisp_engine::Completion;

use wisp_host::cell::{Cell, CellConfig, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
use wisp_host::{Activator, ModuleId, Resolver};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

struct Fixed(Vec<std::net::SocketAddr>);
impl Resolver for Fixed {
    fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(self.0.clone())
    }
}

struct Rig {
    cell: Arc<Cell>,
    #[allow(dead_code)]
    fetcher: Arc<FakeFetcher>,
}

fn services_resolving(addr: &str) -> (Arc<Services>, Arc<FakeFetcher>) {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        Box::new(Fixed(vec![addr.parse().unwrap()])),
        1024,
        1 << 30,
    ));
    let svc = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    (svc, fetcher)
}

fn services() -> (Arc<Services>, Arc<FakeFetcher>) {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        Box::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])),
        1024,
        1 << 30,
    ));
    let svc = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    (svc, fetcher)
}

fn rig(cfg: CellConfig) -> Rig {
    let (svc, fetcher) = services();
    let cell = Cell::new(cfg, svc);
    Rig { cell, fetcher }
}

fn rig_resolving(cfg: CellConfig, addr: &str) -> Rig {
    let (svc, fetcher) = services_resolving(addr);
    let cell = Cell::new(cfg, svc);
    Rig { cell, fetcher }
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

const T: Duration = Duration::from_secs(10);

const PARK_MS: u64 = 600_000;

#[test]
fn parked_instance_cpu_use_stays_within_the_test_budget() {
    const N: usize = 1_000;
    let seconds: u64 = std::env::var("WISP_PARK_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let r = rig(CellConfig {
        exec_threads: 8,
        max_hot: 8,
        admission_queue: 4096,
        max_idle_per_module: N + 16,
        ..CellConfig::for_test("c3")
    });
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("parker", "v1"),
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act,
            manifest("parker", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();

    // Baseline first: what does an *empty* cell cost? Reactor threads, the
    // timer wheel and the idle sweeper all run whether or not anything is
    // parked, and claim C3 is about the *marginal* cost of an instance, not
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
        let mut rec = http(&r.cell, &module, "/park");
        rec.dispatch_id = i as u64 + 1;
        pending.push(r.cell.submit(
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
            || r.cell.resident_count() >= N && r.cell.hot_count() == 0,
            Duration::from_secs(30)
        ),
        "only {} of {N} instances parked (hot={})",
        r.cell.resident_count(),
        r.cell.hot_count()
    );
    let parked = r.cell.metrics().parked;
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
        "empty cell {:.1} ms/s; {N} parked for {secs:.1}s -> {:.1} ms total          ({cores_used:.4} cores), marginal {per_instance_us_per_s:.2} µs/instance/s",
        baseline_ns_per_s / 1e6,
        cpu_ns as f64 / 1e6,
    );

    // "≈0" made falsifiable, twice over.
    //
    // The marginal bound is the claim: a parked instance holds no thread and
    // no stack, so the only CPU it can cost is whatever the cell spends
    // noticing it exists. At 10 µs/instance/s, G1's 5,000 instances would cost
    // 5% of one core — visible but not structural. At 100 µs/s it would be
    // half a core and the economics would be different.
    assert!(
        per_instance_us_per_s < 15.0,
        "each parked instance costs {per_instance_us_per_s:.1} µs/s of cell CPU; parking is not free"
    );
    // And the absolute, so a regression in the fixed overhead is caught too.
    assert!(
        cores_used < 0.03,
        "1,000 parked instances used {cores_used:.4} cores, exceeding the test budget"
    );

    let meters = r.cell.meters(&module).unwrap();
    let cpu_ms = meters.cpu_ms();
    eprintln!("tenant recorded {cpu_ms:.3} CPU-ms for {N} dispatches + {seconds}s of parking");
    assert!(
        cpu_ms < 200.0,
        "recorded {cpu_ms} CPU-ms; the park window must contribute nothing"
    );
    assert!(meters.observed_park_ms() == 0 || meters.cpu_ns() > 0);
    r.cell.shutdown();
}

#[test]
fn a_budget_killed_spinner_does_not_delay_another_tenants_wakes() {
    let r = rig(CellConfig {
        exec_threads: 4,
        max_hot: 4,
        ..CellConfig::for_test("c7")
    });

    let spinner = Arc::new(FakeActivator::named(
        ModuleId::new("spinner", "v1"),
        FakeBehaviour::program([Step::Spin]),
    ));
    let spin_mod = spinner.module().clone();
    r.cell
        .register_module(
            "noisy",
            spinner,
            manifest("spinner", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();

    let victim = Arc::new(FakeActivator::named(
        ModuleId::new("victim", "v1"),
        FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", PARK_MS)],
            },
            Step::Done {
                status: 200,
                body: b"woken".to_vec(),
            },
        ]),
    ));
    let victim_mod = victim.module().clone();
    r.cell
        .register_module(
            "quiet",
            victim,
            manifest("victim", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    // Baseline: how fast is a wake with nobody spinning?
    let baseline = measure_wake(&r, &victim_mod);

    // Now saturate two of the four execution-pool threads with spinners, each
    // burning its full 50 ms budget before being killed.
    let mut spins = Vec::new();
    for i in 0..2 {
        let mut rec = http(&r.cell, &spin_mod, "/spin");
        rec.dispatch_id = 1000 + i;
        rec.cpu_budget_ms = 50;
        spins.push(
            r.cell
                .submit(
                    &spin_mod,
                    Target::Handler {
                        route: "/spin".into(),
                    },
                    rec,
                )
                .unwrap(),
        );
    }
    assert!(
        wait_for(|| r.cell.hot_count() >= 2, Duration::from_secs(5)),
        "spinners never went Hot"
    );

    let under_load = measure_wake(&r, &victim_mod);

    for s in spins {
        let out = s.recv_timeout(T).expect("spinner never finished");
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "the spinner was not killed at its budget"
        );
        assert_eq!(out.status, 503);
        let trap = out.trap.unwrap();
        assert!(trap.starts_with("budget_exceeded"), "wrong trap: {trap}");
        assert!(
            trap.contains("without unwinding"),
            "kill was not abrupt: {trap}"
        );
        // It really did burn its budget rather than being rejected cheaply.
        assert!(
            out.cpu_ns >= 40_000_000,
            "spinner only burned {} ms; it was not actually spinning",
            out.cpu_ns / 1_000_000
        );
    }

    eprintln!(
        "C7: victim wake {} µs idle -> {} µs while two tenants spin at their budget",
        baseline.as_micros(),
        under_load.as_micros()
    );
    assert!(
        under_load < Duration::from_millis(25),
        "wake took {under_load:?} while a neighbour spun; the spinner is blocking other tenants"
    );

    let m = r.cell.metrics();
    assert_eq!(m.tenants["quiet"].traps, 0, "the victim tenant was harmed");
    assert_eq!(m.tenants["noisy"].traps, 2);
    assert_eq!(m.budget_kills, 2);
    r.cell.shutdown();
}

/// One park-then-wake round trip for the victim tenant, measured from
/// "complete the host future" to "the response arrived".
fn measure_wake(r: &Rig, module: &ModuleId) -> Duration {
    let before: std::collections::HashSet<u64> =
        r.cell.futures().live_future_ids().into_iter().collect();
    let rec = http(&r.cell, module, "/wake");
    let rx = r
        .cell
        .submit(
            module,
            Target::Handler {
                route: "/wake".into(),
            },
            rec,
        )
        .unwrap();

    // Wait for it to park with a future outstanding. Parking is a *return*
    // (invariant 3), so from here the victim holds no thread at all.
    let id = {
        let mut found = None;
        assert!(
            wait_for(
                || {
                    found = r
                        .cell
                        .futures()
                        .live_future_ids()
                        .into_iter()
                        .find(|i| !before.contains(i));
                    found.is_some()
                },
                Duration::from_secs(5)
            ),
            "the victim never registered a future"
        );
        found.unwrap()
    };

    let t0 = Instant::now();
    r.cell.complete_future(id, ResultRecord::Ok(b"v".to_vec()));
    let out = rx.recv_timeout(T).expect("the victim never woke");
    let elapsed = t0.elapsed();
    assert_eq!(out.status, 200, "victim failed: {:?}", out.trap);
    assert_eq!(out.slices, 2, "expected dispatch + one wake");
    elapsed
}

#[test]
fn fresh_leaves_no_residue_reuse_does() {
    let r = rig(CellConfig::for_test("residue"));

    for (app, lifecycle) in [
        ("reuse-app", HandlerLifecycle::Reuse),
        ("fresh-app", HandlerLifecycle::Fresh),
    ] {
        let act = Arc::new(FakeActivator::named(
            ModuleId::new(app, "v1"),
            FakeBehaviour::done().with_lazy_init_probe(),
        ));
        let module = act.module().clone();
        r.cell
            .register_module("acme", act, manifest(app, lifecycle, Limits::default()))
            .unwrap();

        let mut observed = Vec::new();
        for i in 0..25 {
            let mut rec = http(&r.cell, &module, "/probe");
            rec.dispatch_id = i;
            let out = r
                .cell
                .dispatch_blocking(
                    &module,
                    Target::Handler {
                        route: "/probe".into(),
                    },
                    rec,
                    T,
                )
                .unwrap();
            assert_eq!(out.status, 200, "{app} failed: {:?}", out.trap);
            observed.push(out.body[0]);
        }

        match lifecycle {
            HandlerLifecycle::Fresh => {
                assert!(
                    observed.iter().all(|v| *v == 0),
                    "§2.6: a `fresh` dispatch observed residue: {observed:?}"
                );
                let m = r.cell.meters(&module).unwrap();
                assert_eq!(m.dispatches(), 25);
            }
            HandlerLifecycle::Reuse => {
                assert_eq!(observed[0], 0, "the first dispatch cannot observe residue");
                assert!(
                    observed[1..].iter().any(|v| *v == 1),
                    "§2.6: `reuse` must let per-instance memoization pay off: {observed:?}"
                );
                let m = r.cell.meters(&module).unwrap();
            }
        }
    }
    r.cell.shutdown();
}

#[test]
fn the_pending_futures_cap_traps_the_guest() {
    let free = rig(CellConfig {
        ..CellConfig::for_test("cap-free")
    });
    let paid = rig(CellConfig::for_test("cap-paid"));
    let r = &free;
    let limit = Limits {
        module_bytes_compressed: 5242880,
        instance_memory_max: 67108864,
        cpu_ms_per_http_dispatch: 10,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .pending_futures_per_dispatch;
    assert_eq!(limit, 6);

    let act = Arc::new(FakeActivator::named(
        ModuleId::new("greedy", "v1"),
        FakeBehaviour::program([Step::Park {
            imports: (0..limit + 3)
                .map(|_| ("clock_deadline_in", PARK_MS))
                .collect(),
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "limited-app",
            act,
            manifest(
                "greedy",
                HandlerLifecycle::Reuse,
                Limits {
                    module_bytes_compressed: 5242880,
                    instance_memory_max: 67108864,
                    cpu_ms_per_http_dispatch: 10,
                    subrequests_per_dispatch: 20,
                    pending_futures_per_dispatch: 6,
                },
            ),
        )
        .unwrap();

    let mut rec = http(&r.cell, &module, "/greedy");
    rec.dispatch_id = 1;
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/greedy".into(),
            },
            rec,
            T,
        )
        .unwrap();

    assert_eq!(
        out.completion,
        Completion::Trapped,
        "the cap did not trap the guest"
    );
    assert_eq!(out.status, 500);
    assert_eq!(r.cell.metrics().pending_cap_rejections, 1);

    let ok = Arc::new(FakeActivator::named(
        ModuleId::new("higher-limit-app", "v1"),
        FakeBehaviour::program([Step::Park {
            imports: (0..limit + 3)
                .map(|_| ("clock_deadline_in", PARK_MS))
                .collect(),
        }]),
    ));
    let ok_mod = ok.module().clone();
    paid.cell
        .register_module(
            "acme",
            ok,
            manifest(
                "higher-limit-app",
                HandlerLifecycle::Reuse,
                Limits::default(),
            ),
        )
        .unwrap();
    let mut rec = http(&paid.cell, &ok_mod, "/ok");
    rec.dispatch_id = 2;
    let rx = paid
        .cell
        .submit(
            &ok_mod,
            Target::Handler {
                route: "/ok".into(),
            },
            rec,
        )
        .unwrap();
    assert!(
        wait_for(
            || paid.cell.futures().live_futures() >= (limit + 3) as usize,
            Duration::from_secs(5)
        ),
        "the higher configured future cap was not applied"
    );
    assert_eq!(paid.cell.metrics().pending_cap_rejections, 0);
    drop(rx);
    free.cell.shutdown();
    paid.cell.shutdown();
}

#[test]
fn the_breaker_fails_a_repeat_offender_fast_without_entering_the_guest() {
    let r = rig(CellConfig {
        exec_threads: 2,
        max_hot: 2,
        ..CellConfig::for_test("breaker")
    });
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("hangs", "v1"),
        FakeBehaviour::program([Step::Spin]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act.clone(),
            manifest("hangs", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();

    let mut killed_cpu = Vec::new();
    for i in 0..3 {
        let mut rec = http(&r.cell, &module, "/hang");
        rec.dispatch_id = i;
        rec.cpu_budget_ms = 10;
        let out = r
            .cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/hang".into(),
                },
                rec,
                T,
            )
            .unwrap();
        assert_eq!(out.completion, Completion::BudgetKill);
        killed_cpu.push(out.cpu_ns);
    }
    // Every kill cost real CPU — that is what the breaker exists to stop
    // paying.
    assert!(killed_cpu.iter().all(|c| *c > 5_000_000), "{killed_cpu:?}");
    let activations_before = act.activation_count();

    // The fourth request is refused without entering the guest at all.
    let mut rec = http(&r.cell, &module, "/hang");
    rec.dispatch_id = 99;
    rec.cpu_budget_ms = 10;
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/hang".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(out.status, 503);
    assert_eq!(out.trap.as_deref(), Some("breaker_open"));
    assert_eq!(out.cpu_ns, 0, "the breaker burned CPU to say no");
    assert_eq!(
        act.activation_count(),
        activations_before,
        "the breaker activated an instance to reject the request"
    );
    assert_eq!(r.cell.metrics().breaker_rejections, 1);
    r.cell.shutdown();
}

#[test]
fn the_metrics_endpoint_decomposes_the_lifecycle() {
    let r = rig(CellConfig::for_test("metrics"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("m", "v1"),
        FakeBehaviour::done(),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act,
            manifest("m", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();
    for i in 0..5 {
        let mut rec = http(&r.cell, &module, "/");
        rec.dispatch_id = i;
        r.cell
            .dispatch_blocking(&module, Target::Handler { route: "/".into() }, rec, T)
            .unwrap();
    }
    let text = r.cell.metrics_text();
    for needle in [
        "wisp_cell_dispatches 5",
        "wisp_cell_instances_live",
        "wisp_cell_instances_parked",
        "wisp_cell_instances_hibernated",
        "wisp_cell_activation_p50_us",
        "wisp_cell_wake_p50_us",
        "wisp_cell_dirty_pages_p50",
        "wisp_cell_delta_bytes",
        "wisp_cell_trap_rate",
        "wisp_tenant_dispatches{tenant=\"acme\"}",
    ] {
        assert!(
            text.contains(needle),
            "the scrape is missing `{needle}`:\n{text}"
        );
    }
    let json: serde_json::Value = serde_json::from_str(&r.cell.metrics_json()).unwrap();
    assert_eq!(json["dispatches"], 5);
    assert_eq!(json["served_activated"], 1);
    assert_eq!(json["served_warm"], 4);
    r.cell.shutdown();
}

#[test]
fn wait_until_keeps_the_instance_alive_past_the_response() {
    let r = rig(CellConfig::for_test("waituntil"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("bg", "v1"),
        FakeBehaviour::program([Step::WaitUntil {
            import: "clock_deadline_in",
            scalar: PARK_MS,
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act,
            manifest("bg", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    let rec = http(&r.cell, &module, "/bg");
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/bg".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(
        out.status, 202,
        "the response must not wait for the background work"
    );
    assert_eq!(out.body, b"accepted");

    // The response is delivered, the background future is still outstanding,
    // and the instance is therefore still resident and Parked rather than
    // reset back into the pool.
    let instance = out.instance;
    assert!(
        r.cell.futures().wait_until_outstanding(instance),
        "§2.2: the WaitUntil work was dropped"
    );
    assert_eq!(
        r.cell.instance_state(instance),
        Some(wisp_host::InstanceState::Parked)
    );
    r.cell.shutdown();
}

#[test]
fn logging_is_charged_to_the_dispatch_that_logs() {
    let r = rig(CellConfig::for_test("logcpu"));

    let quiet = Arc::new(FakeActivator::named(
        ModuleId::new("quiet-logger", "v1"),
        FakeBehaviour::program([Step::Log {
            bytes: 64,
            times: 1,
        }]),
    ));
    let loud = Arc::new(FakeActivator::named(
        ModuleId::new("loud-logger", "v1"),
        FakeBehaviour::program([Step::Log {
            bytes: 64,
            times: 2_000,
        }]),
    ));
    let qm = quiet.module().clone();
    let lm = loud.module().clone();
    r.cell
        .register_module(
            "acme",
            quiet,
            manifest("quiet-logger", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();
    r.cell
        .register_module(
            "acme",
            loud,
            manifest("loud-logger", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    let q = r
        .cell
        .dispatch_blocking(
            &qm,
            Target::Handler { route: "/l".into() },
            http(&r.cell, &qm, "/l"),
            T,
        )
        .unwrap();
    let l = r
        .cell
        .dispatch_blocking(
            &lm,
            Target::Handler { route: "/l".into() },
            http(&r.cell, &lm, "/l"),
            T,
        )
        .unwrap();

    eprintln!(
        "§10: 1 log line = {} µs CPU, 2000 = {} µs",
        q.cpu_ns / 1000,
        l.cpu_ns / 1000
    );
    assert!(
        l.cpu_ns > q.cpu_ns * 4,
        "logging 2000 lines cost {} ns vs {} ns for one; logs are close to free",
        l.cpu_ns,
        q.cpu_ns
    );
    assert!(r.cell.meters(&lm).unwrap().log_cpu_ns() > 0);
    r.cell.shutdown();
}

#[test]
fn a_guest_cannot_fetch_a_private_address() {
    let r = rig_resolving(CellConfig::for_test("ssrf"), "169.254.169.254:80");
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("prober", "v1"),
        FakeBehaviour::program([Step::Fetch {
            // The cloud metadata endpoint: the target this control exists for.
            url: "http://169.254.169.254/latest/meta-data/".into(),
            times: 1,
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act.clone(),
            manifest("prober", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    let rx = r
        .cell
        .submit(
            &module,
            Target::Handler {
                route: "/probe".into(),
            },
            http(&r.cell, &module, "/probe"),
        )
        .unwrap();
    assert!(
        wait_for(|| !act.wakes().is_empty(), Duration::from_secs(5)),
        "the guest was never told what happened to its fetch"
    );
    let (_, res) = act.wakes()[0].clone();
    match res {
        ResultRecord::Err(FutureError::Denied(m)) => {
            assert!(m.contains("169.254.169.254"), "{m}");
        }
        other => panic!("the metadata endpoint was reachable: {other:?}"),
    }
    assert!(
        r.fetcher.connected_to.lock().is_empty(),
        "the host dialled a link-local address"
    );
    drop(rx);
    r.cell.shutdown();
}

#[test]
fn clones_of_one_template_never_share_entropy() {
    let r = rig(CellConfig::for_test("entropy"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("rng", "v1"),
        FakeBehaviour::program([Step::Entropy { bytes: 32 }]),
    ));
    let module = act.module().clone();
    // `fresh`: every dispatch is a pristine clone of the same template, which
    // is the worst case for this property.
    r.cell
        .register_module(
            "acme",
            act.clone(),
            manifest("rng", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    for i in 0..50 {
        let mut rec = http(&r.cell, &module, "/rng");
        rec.dispatch_id = i;
        r.cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/rng".into(),
                },
                rec,
                T,
            )
            .unwrap();
    }
    let draws = act.entropy.lock().clone();
    assert_eq!(draws.len(), 50);
    let unique: std::collections::HashSet<Vec<u8>> = draws.into_iter().collect();
    assert_eq!(
        unique.len(),
        50,
        "clones of one template shared an RNG stream"
    );
    assert!(
        unique.iter().all(|d| d.iter().any(|b| *b != 0)),
        "entropy was zero-filled"
    );
    r.cell.shutdown();
}
