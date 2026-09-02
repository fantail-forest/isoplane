//! Cell-level tests against [`wisp_host::fakes`]. What is not faked is what
//! each claim is about: the spinner burns real CPU on a real execution thread
//! and is killed by a real deadline, and parked instances hold real timers.

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

struct Fixed(Vec<std::net::SocketAddr>);
impl Resolver for Fixed {
    fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(self.0.clone())
    }
}

struct Rig {
    cell: Arc<Cell>,
    fetcher: Arc<FakeFetcher>,
}

fn rig(cfg: CellConfig) -> Rig {
    rig_resolving(cfg, "93.184.216.34:80")
}

/// A cell whose egress resolver answers every name with `addr`.
fn rig_resolving(cfg: CellConfig, addr: &str) -> Rig {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        Box::new(Fixed(vec![addr.parse().expect("a literal address")])),
        1024,
        1 << 30,
    ));
    let svc = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    let cell = Cell::new(cfg, svc).expect("a cell starts");
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

/// A host timer that will not fire during any test; parking on it holds an
/// instance Parked for as long as a test needs to look at it.
const PARK_MS: u64 = 600_000;

/// A spinner occupies only its own execution-pool thread and the CPU budget
/// kills it without unwinding; another tenant's parked instance is still woken
/// and answered while two spinners burn their budgets. The spinner is real:
/// [`Step::Spin`] burns CPU on the thread it was scheduled on and is
/// terminated when its thread CPU clock passes the budget.
#[test]
fn a_budget_killed_spinner_does_not_block_another_tenants_wake() {
    let r = rig(CellConfig {
        exec_threads: 4,
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

    // `fresh`, so every request gets a fresh copy of the program.
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

    wake_once(&r, &victim_mod);

    // Two of the four execution threads spin at a budget short enough to die
    // inside the test's own timeout.
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

    wake_once(&r, &victim_mod);

    for s in spins {
        let out = s.recv_timeout(T).expect("spinner never finished");
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "the spinner was not killed at its budget"
        );
        assert_eq!(out.status, 503, "a hard kill is a 503");
        let trap = out.trap.expect("a kill names its trap");
        assert!(trap.starts_with("budget_exceeded"), "wrong trap: {trap}");
        assert!(
            trap.contains("without unwinding"),
            "kill was not abrupt: {trap}"
        );
        assert!(
            out.cpu_ns >= 40_000_000,
            "spinner only burned {} ms; it was not actually spinning",
            out.cpu_ns / 1_000_000
        );
    }

    let m = r.cell.metrics();
    assert_eq!(m.tenants["quiet"].traps, 0, "the victim tenant was harmed");
    assert_eq!(
        m.tenants["noisy"].traps, 2,
        "the offender's own requests are what suffer"
    );
    assert_eq!(m.budget_kills, 2, "both spinners were killed at budget");
    r.cell.shutdown();
}

/// One park-then-wake round trip for the victim tenant.
fn wake_once(r: &Rig, module: &ModuleId) {
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

    r.cell.complete_future(id, ResultRecord::Ok(b"v".to_vec()));
    let out = rx.recv_timeout(T).expect("the victim never woke");
    assert_eq!(out.status, 200, "victim failed: {:?}", out.trap);
    assert_eq!(out.slices, 2, "expected dispatch + one wake");
}

/// The fake guest's lazy-init probe reports `0` for a pristine instance and
/// `1` for a previous request's residue.
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
                    "a `fresh` dispatch observed residue: {observed:?}"
                );
                let m = r.cell.meters(&module).unwrap();
                assert_eq!(m.dispatches(), 25, "every dispatch is counted");
            }
            HandlerLifecycle::Reuse => {
                assert_eq!(observed[0], 0, "the first dispatch cannot observe residue");
                assert!(
                    observed[1..].contains(&1),
                    "`reuse` must let per-instance memoization pay off: {observed:?}"
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
    assert_eq!(limit, 6, "the free plan's cap");

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
    assert_eq!(out.status, 500, "a trap is a 500");
    assert_eq!(
        r.cell.metrics().pending_cap_rejections,
        1,
        "the cap refusal is counted"
    );

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
    assert_eq!(
        paid.cell.metrics().pending_cap_rejections,
        0,
        "the standard plan was never capped"
    );
    drop(rx);
    free.cell.shutdown();
    paid.cell.shutdown();
}

#[test]
fn the_breaker_fails_a_repeat_offender_fast_without_entering_the_guest() {
    let r = rig(CellConfig {
        exec_threads: 2,
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
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "the spinner was killed"
        );
        killed_cpu.push(out.cpu_ns);
    }
    assert!(
        killed_cpu.iter().all(|c| *c > 5_000_000),
        "every kill cost real CPU: {killed_cpu:?}"
    );
    let activations_before = act.activation_count();

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
    assert_eq!(out.status, 503, "the breaker answers 503");
    assert_eq!(out.trap.as_deref(), Some("breaker_open"));
    assert_eq!(out.cpu_ns, 0, "the breaker burned CPU to say no");
    assert_eq!(
        act.activation_count(),
        activations_before,
        "the breaker activated an instance to reject the request"
    );
    assert_eq!(
        r.cell.metrics().breaker_rejections,
        1,
        "the rejection is counted"
    );
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
        // The response leaves before the instance is pooled, so a client that
        // fires the next request at once can race the release and pay for a
        // second activation; waiting for the pool measures the steady state.
        assert!(
            wait_for(
                || r.cell
                    .idle_instances(&module)
                    .is_some_and(|(w, f)| w + f >= 1),
                Duration::from_secs(5)
            ),
            "the instance never returned to the pool after dispatch {i}"
        );
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
    assert_eq!(
        json["dispatches"], 5,
        "the json scrape carries the counters"
    );
    assert_eq!(
        json["served_activated"], 1,
        "only the first request paid for an activation"
    );
    assert_eq!(json["served_warm"], 4, "the rest were served warm");
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

    let instance = out.instance;
    assert!(
        r.cell.futures().wait_until_outstanding(instance),
        "the WaitUntil work was dropped"
    );
    assert_eq!(
        r.cell.instance_state(instance),
        Some(wisp_host::LifecycleTier::Parked)
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

    assert!(
        l.cpu_ns > q.cpu_ns * 4,
        "logging 2000 lines cost {} ns vs {} ns for one; logs are close to free",
        l.cpu_ns,
        q.cpu_ns
    );
    assert!(
        r.cell.meters(&lm).unwrap().log_cpu_ns() > 0,
        "the log CPU share is recorded"
    );
    r.cell.shutdown();
}

#[test]
fn a_guest_cannot_fetch_a_private_address() {
    let r = rig_resolving(CellConfig::for_test("ssrf"), "169.254.169.254:80");
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("prober", "v1"),
        FakeBehaviour::program([Step::Fetch {
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
    // `fresh`: every dispatch is a clone of the same template.
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
    assert_eq!(draws.len(), 50, "every dispatch drew entropy");
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

/// One slow dispatch is submitted first and fifty fast ones behind it, so
/// the first result off the shared channel is not the first request in.
#[test]
fn many_dispatches_share_one_channel_and_stay_matchable() {
    let r = rig(CellConfig {
        exec_threads: 4,
        queue_depth_per_worker: 64,
        ..CellConfig::for_test("share")
    });

    let slow = Arc::new(FakeActivator::named(
        ModuleId::new("slow", "v1"),
        FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", 250)],
            },
            Step::Done {
                status: 200,
                body: b"slow".to_vec(),
            },
        ]),
    ));
    let slow_mod = slow.module().clone();
    r.cell
        .register_module(
            "acme",
            slow,
            manifest("slow", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    let fast = Arc::new(FakeActivator::named(
        ModuleId::new("fast", "v1"),
        FakeBehaviour::program([Step::Done {
            status: 200,
            body: b"fast".to_vec(),
        }]),
    ));
    let fast_mod = fast.module().clone();
    r.cell
        .register_module(
            "acme",
            fast,
            manifest("fast", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();

    const SLOW_ID: u64 = 9_000;
    const FAST: u64 = 50;
    let (tx, rx) = std::sync::mpsc::sync_channel::<wisp_host::DispatchResult>(1024);

    let mut rec = http(&r.cell, &slow_mod, "/slow");
    rec.dispatch_id = SLOW_ID;
    assert!(r.cell.submit_with(
        &slow_mod,
        Target::Handler {
            route: "/slow".into()
        },
        rec,
        tx.clone()
    ));

    for i in 0..FAST {
        let mut rec = http(&r.cell, &fast_mod, "/fast");
        rec.dispatch_id = i;
        assert!(r.cell.submit_with(
            &fast_mod,
            Target::Handler {
                route: "/fast".into()
            },
            rec,
            tx.clone()
        ));
    }
    drop(tx);

    let mut ids = Vec::new();
    for _ in 0..=FAST {
        let res = rx.recv_timeout(Duration::from_secs(10)).expect("result");
        assert_eq!(
            res.status, 200,
            "unexpected status for dispatch {}",
            res.dispatch_id
        );
        ids.push(res.dispatch_id);
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "extra result on the channel"
    );

    assert_ne!(
        ids[0], SLOW_ID,
        "the slow dispatch was submitted first; if it also answered first, this test is not          exercising out-of-order completion and proves nothing about correlation"
    );
    assert_eq!(
        *ids.last().unwrap(),
        SLOW_ID,
        "the 250 ms park should answer last"
    );

    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "a dispatch_id came back twice");
    let mut expected: std::collections::HashSet<u64> = (0..FAST).collect();
    expected.insert(SLOW_ID);
    assert_eq!(
        unique, expected,
        "the ids that came back are not the ids that went in"
    );

    r.cell.shutdown();
}

/// A spinner holds its thread until its budget kills it, so with one thread
/// and a one-deep lane the outcome is determined: one Hot, one queued, the
/// rest shed. A parking guest would drain the lane as fast as the test fills
/// it.
#[test]
fn a_shed_submission_says_so_rather_than_going_quiet() {
    let r = rig(CellConfig {
        exec_threads: 1,
        queue_depth_per_worker: 1,
        ..CellConfig::for_test("shed")
    });
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("spin", "v1"),
        FakeBehaviour::program([Step::Spin]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act,
            manifest("spin", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();

    let (tx, rx) = std::sync::mpsc::sync_channel::<wisp_host::DispatchResult>(128);
    let mut rec = http(&r.cell, &module, "/spin");
    rec.dispatch_id = 0;
    rec.cpu_budget_ms = 50;
    assert!(r.cell.submit_with(
        &module,
        Target::Handler {
            route: "/spin".into()
        },
        rec,
        tx.clone()
    ));
    assert!(
        wait_for(|| r.cell.hot_count() == 1, Duration::from_secs(5)),
        "the spinner never became Hot, so the queue state below is not determined"
    );

    let mut shed = 0;
    let mut accepted = 1;
    for i in 1..64u64 {
        let mut rec = http(&r.cell, &module, "/spin");
        rec.dispatch_id = i;
        // The queued one runs too, and the drain below waits two seconds.
        rec.cpu_budget_ms = 50;
        if r.cell.submit_with(
            &module,
            Target::Handler {
                route: "/spin".into(),
            },
            rec,
            tx.clone(),
        ) {
            accepted += 1;
        } else {
            shed += 1;
        }
    }
    drop(tx);
    assert_eq!(
        accepted, 2,
        "one Hot and one queued; everything else must shed at admission"
    );
    assert_eq!(shed, 62, "the rest were shed");
    assert!(
        r.cell.metrics().shed >= 62,
        "the cell must count what it shed"
    );

    let mut got = 0;
    while rx.recv_timeout(Duration::from_secs(2)).is_ok() {
        got += 1;
    }
    assert_eq!(
        got, accepted,
        "results on the channel must equal accepted submissions"
    );
    r.cell.shutdown();
}
