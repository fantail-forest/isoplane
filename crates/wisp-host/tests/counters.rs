use std::sync::Arc;
use std::time::Duration;

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::status::TrapCode;

use wisp_host::cell::{Cell, CellConfig, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
use wisp_host::instances::EnterError;
use wisp_host::{Activator, LifecycleTier, ModuleId, Resolver};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const T: Duration = Duration::from_secs(10);
/// A host timer long enough that it never fires during a test. Parking on it
/// is how a test holds an instance Parked long enough to look at it.
const PARK_MS: u64 = 600_000;

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
    let fetcher = Arc::new(FakeFetcher::default());
    let svc = Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])),
            1024,
            1 << 30,
        )),
        fetcher: fetcher.clone(),
    });
    Rig {
        cell: Cell::new(cfg, svc),
        fetcher,
    }
}

fn manifest(name: &str, lifecycle: HandlerLifecycle) -> AppManifest {
    AppManifest {
        name: name.into(),
        handlers: HandlerConfig { lifecycle },
        bindings: Bindings::default(),
        limits: Limits::default(),
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
    let t0 = std::time::Instant::now();
    while t0.elapsed() < within {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// Register one fake module and hand back its id.
fn app(r: &Rig, name: &str, behaviour: FakeBehaviour) -> ModuleId {
    let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act, manifest(name, HandlerLifecycle::Reuse))
        .unwrap();
    module
}

#[test]
fn a_hard_kill_is_not_counted_as_a_trap() {
    let r = rig(CellConfig {
        exec_threads: 4,
        max_hot: 4,
        ..CellConfig::for_test("traps")
    });
    let trapper = app(
        &r,
        "trapper",
        FakeBehaviour::program([Step::Trap(TrapCode::Panic)]),
    );
    let spinner = app(&r, "spinner", FakeBehaviour::program([Step::Spin]));

    let mut rec = http(&r.cell, &trapper, "/trap");
    rec.cpu_budget_ms = 50;
    let out = r
        .cell
        .dispatch_blocking(
            &trapper,
            Target::Handler {
                route: "/trap".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(out.status, 500, "a trap is a 500");

    let mut rec = http(&r.cell, &spinner, "/spin");
    rec.cpu_budget_ms = 50;
    let out = r
        .cell
        .dispatch_blocking(
            &spinner,
            Target::Handler {
                route: "/spin".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(out.status, 503, "a hard budget kill is a 503");

    let m = r.cell.metrics();
    assert_eq!(m.hard_kills, 1);
    assert_eq!(
        m.traps, 1,
        "the budget kill was counted as a trap as well as a kill"
    );
    assert_eq!(m.tombstones, 2, "both events tombstone");
    assert_eq!(
        m.tombstones,
        m.hard_kills + m.traps,
        "§6.4's identity. If `traps` overshoots by exactly `hard_kills` the two are sharing an \
         increment again"
    );
    r.cell
        .metrics_raw()
        .tombstone_invariant()
        .expect("§6.4 invariant");

    // The tenant-level meters keep the same split, so a per-tenant trap rate
    // is not inflated by kills either.
    assert_eq!(m.tenants["acme"].traps, 2, "both are faults for the tenant");
    r.cell.shutdown();
}

#[test]
fn a_host_call_into_a_tombstone_is_refused_and_counted() {
    let r = rig(CellConfig::for_test("tombstone-refusal"));
    let module = app(
        &r,
        "trapper",
        FakeBehaviour::program([Step::Trap(TrapCode::Panic)]),
    );

    let rec = http(&r.cell, &module, "/trap");
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/trap".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(out.status, 500);
    let killed = out.instance;
    assert_eq!(
        r.cell.tombstoned_instances(),
        vec![killed],
        "the kill left no tombstone to call"
    );
    assert_eq!(
        r.cell.metrics().tombstone_refusals,
        0,
        "nothing has called it yet"
    );

    let refused = r.cell.with_instance(killed, |i| i.is_parked());
    assert!(
        matches!(refused, Err(EnterError::Tombstoned)),
        "got {refused:?}"
    );
    assert_eq!(
        r.cell.metrics().tombstone_refusals,
        1,
        "the refusal happened and the negative control did not move"
    );

    // It counts refusals, not tombstones: asking twice is two refusals.
    let _ = r.cell.with_instance(killed, |i| i.is_parked());
    assert_eq!(r.cell.metrics().tombstone_refusals, 2);

    // And an instance this cell never had is "not here", which is a different
    // fact and must not inflate the control.
    let missing = r
        .cell
        .with_instance(wisp_host::InstanceId(u64::MAX), |i| i.is_parked());
    assert!(
        matches!(missing, Err(EnterError::NotResident(_))),
        "got {missing:?}"
    );
    assert_eq!(
        r.cell.metrics().tombstone_refusals,
        2,
        "a missing instance is not a tombstone"
    );
    r.cell.shutdown();
}

#[test]
fn the_per_dispatch_histograms_can_be_taken_off_the_dispatch_path() {
    let r = rig(CellConfig {
        telemetry_sample_every: 0,
        ..CellConfig::for_test("no-hist")
    });
    let module = app(&r, "quick", FakeBehaviour::done());

    for n in 0..5 {
        let rec = http(&r.cell, &module, "/go");
        let out = r
            .cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/go".into(),
                },
                rec,
                T,
            )
            .unwrap();
        assert_eq!(out.status, 200, "dispatch {n} failed");
    }

    let raw = r.cell.metrics_raw();
    assert_eq!(raw.per_dispatch_sampling(), 0);
    // `respond` runs before `finish_and_release`, so the response can arrive
    // a hair before the request timer is recorded. The count is the assertion;
    // the wait is only so the assertion is not a race.
    assert!(
        wait_for(|| raw.request_us.count() == 5, Duration::from_secs(5)),
        "turning sampling off must not turn the metric into a zero — the count is what §10 needs \
         to keep the rate honest, and it read {}",
        raw.request_us.count()
    );
    assert_eq!(
        raw.request_us.p50(),
        0,
        "with no retained samples there is no percentile to give"
    );

    let m = r.cell.metrics();
    assert_eq!(m.dispatches, 5, "the rest of §10 is unaffected");
    assert!(r.cell.metrics_text().contains("wisp_cell_request_p50_us 0"));

    // The default is every sample, and the same workload then has a
    // percentile — which is what makes the pair a difference and not a
    // different binary.
    let sampled = rig(CellConfig::for_test("hist"));
    let module = app(&sampled, "quick", FakeBehaviour::done());
    for _ in 0..5 {
        let rec = http(&sampled.cell, &module, "/go");
        sampled
            .cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/go".into(),
                },
                rec,
                T,
            )
            .unwrap();
    }
    assert_eq!(sampled.cell.metrics_raw().per_dispatch_sampling(), 1);
    assert!(wait_for(
        || sampled.cell.metrics_raw().request_us.count() == 5,
        Duration::from_secs(5)
    ));
    assert_eq!(
        sampled.cell.metrics_raw().request_us.retained(),
        5,
        "the default configuration must retain samples; without them the comparison this test \
         exists to make is between two identical zeros"
    );
    assert_eq!(
        raw.request_us.retained(),
        0,
        "at sampling 0 the window is empty — which is what removes the lock from the dispatch \
         path, and the only thing that is given up"
    );
    r.cell.shutdown();
    sampled.cell.shutdown();
}

#[test]
fn per_worker_queues_serve_every_dispatch_exactly_once() {
    for strategy in [
        wisp_host::cell::QueueStructure::Shared,
        wisp_host::cell::QueueStructure::PerWorker,
        wisp_host::cell::QueueStructure::LockFree,
    ] {
        let mut cfg = CellConfig::for_test("qstrat");
        cfg.exec_threads = 8;
        cfg.queue_structure = strategy;
        let r = rig(cfg);
        let module = app(&r, "http", FakeBehaviour::done());

        const BATCHES: u64 = 16;
        const EACH: u64 = 8;
        let mut ok = 0u64;
        for i in 0..BATCHES {
            let path = format!("/batch/{i}");
            let target = Target::Handler {
                route: path.clone(),
            };
            for n in 1..=EACH {
                let mut record = http(&r.cell, &module, &path);
                record.cpu_budget_ms = 50;
                let out = r
                    .cell
                    .dispatch_blocking(&module, target.clone(), record, T)
                    .unwrap_or_else(|e| panic!("{strategy:?}: dispatch {i}/{n} lost: {e:?}"));
                assert_eq!(
                    out.status, 200,
                    "{strategy:?}: dispatch {i}/{n}: {:?}",
                    out.trap
                );
                ok += 1;
            }
        }
        assert_eq!(
            ok,
            BATCHES * EACH,
            "{strategy:?}: every caller must get a reply"
        );

        let m = r.cell.metrics();
        assert_eq!(
            m.dispatches,
            BATCHES * EACH,
            "{strategy:?}: the cell must have run exactly what was submitted — more means a job \
             was popped twice, fewer means one was dropped"
        );
        assert_eq!(
            m.served_warm + m.served_pooled_fresh + m.served_hibernation_wake + m.served_activated,
            m.dispatches,
            "{strategy:?}: the acquisition sources stay exhaustive whichever queue was used"
        );
        assert_eq!(
            m.shed, 0,
            "{strategy:?}: this load is far inside the admission cap"
        );
        r.cell.shutdown();
    }
}

// ---------------------------------------------------------------------------
// The wake router's silent drop
// ---------------------------------------------------------------------------

/// **A completion that cannot be delivered is counted, not swallowed.**
///
/// `CellWake::signal` maps an instance id through a table of `Weak<Slot>`. When
/// the id is absent, or the `Weak` no longer upgrades, the old code took the
/// `if let Some` and fell off the end: no wake enqueued, no counter, no log.
///
/// That is not a no-op. `FutureTable::complete` has *already* run by then — it
/// is what produced the owner id — so the result is sitting in the table marked
/// `Ready` on the owner's queue, and the enqueue that would have sent a worker
/// to collect it is the thing that did not happen. Any dispatch awaiting that
/// future is never answered: on `dispatch_blocking` it waits out its whole
/// timeout, and on the shared-channel path the load generator uses it never
/// learns anything at all.
///
/// The two halves are asserted separately because they fail separately: the
/// counter says the drop was seen, and `has_ready` says the drop stranded
/// something real. Without the second half the counter could be moved by a
/// wake that had nothing behind it.
#[test]
fn a_completion_for_an_instance_that_is_gone_is_counted_rather_than_dropped_in_silence() {
    let r = rig(CellConfig::for_test("wake-dropped"));
    // Never resident, so the wake router's index has no entry for it and
    // `Weak::upgrade` cannot succeed.
    let ghost = wisp_host::InstanceId::alloc();
    let fid = r
        .cell
        .futures()
        .alloc(ghost, 1, "clock_deadline_in", 8)
        .expect("the per-dispatch cap cannot refuse the first future");

    assert_eq!(
        r.cell.metrics().wakes_dropped,
        0,
        "nothing has been dropped yet"
    );
    r.cell
        .complete_future(fid, wisp_abi::result::ResultRecord::Ok(vec![]));

    let m = r.cell.metrics();
    assert_eq!(
        m.wakes_dropped, 1,
        "a completion arrived for an unreachable instance and nothing counted it — which is \
         how a request that simply stops looks from the outside"
    );
    assert_eq!(
        m.wakes, 0,
        "no wake was enqueued, so none may be counted: the two counters have to disagree here \
         or neither of them means anything"
    );
    assert!(
        r.cell.futures().has_ready(ghost),
        "the completion was written into the table before the wake was dropped, so the result \
         is stranded — this is what the counter is a receipt for"
    );
    r.cell.shutdown();
}

/// The control for the test above: a completion that **is** deliverable moves
/// `wakes` and leaves `wakes_dropped` alone.
///
/// A counter that only ever goes up is indistinguishable from one wired to the
/// wrong branch, so the drop count is worth nothing without a case that must
/// not move it.
#[test]
fn a_deliverable_completion_moves_wakes_and_not_the_drop_count() {
    let r = rig(CellConfig::for_test("wake-delivered"));
    let module = app(
        &r,
        "parker",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    );
    let rx = r
        .cell
        .submit(
            &module,
            Target::Handler { route: "/x".into() },
            http(&r.cell, &module, "/x"),
        )
        .unwrap();
    assert!(
        wait_for(
            || r.cell.futures().live_futures() > 0,
            Duration::from_secs(5)
        ),
        "the guest never parked, so there was no completion to deliver"
    );
    let fid = r
        .cell
        .futures()
        .live_future_ids()
        .into_iter()
        .next()
        .expect("one pending future");
    r.cell
        .complete_future(fid, wisp_abi::result::ResultRecord::Ok(vec![]));
    let out = rx
        .recv_timeout(T)
        .expect("the woken dispatch never answered");
    assert_eq!(out.status, 200);

    let m = r.cell.metrics();
    assert_eq!(m.wakes, 1, "the wake was enqueued and must be counted");
    assert_eq!(
        m.wakes_dropped, 0,
        "a live instance's completion was counted as undeliverable"
    );
    r.cell.shutdown();
}
