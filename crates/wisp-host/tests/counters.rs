//! Each cell counter is moved by the event it counts. Where a counter is one
//! half of an identity, the identity is asserted too.

use std::sync::Arc;
use std::time::Duration;

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::status::TrapCode;

use wisp_host::cell::{Cell, CellConfig, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
use wisp_host::slot::EnterError;
use wisp_host::{Activator, LifecycleTier, ModuleId, Resolver};

const T: Duration = Duration::from_secs(10);
/// A host timer that never fires during a test.
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
        cell: Cell::new(cfg, svc).expect("a cell starts"),
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

fn app(r: &Rig, name: &str, behaviour: FakeBehaviour) -> ModuleId {
    let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act, manifest(name, HandlerLifecycle::Reuse))
        .unwrap();
    module
}

/// `tombstones == hard_kills + traps` is checkable only because the two terms
/// are disjoint.
#[test]
fn a_hard_kill_is_not_counted_as_a_trap() {
    let r = rig(CellConfig {
        exec_threads: 4,
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
    assert_eq!(m.hard_kills, 1, "the spin was hard-killed");
    assert_eq!(
        m.traps, 1,
        "the budget kill was counted as a trap as well as a kill"
    );
    assert_eq!(m.tombstones, 2, "both events tombstone");
    assert_eq!(
        m.tombstones,
        m.hard_kills + m.traps,
        "the tombstone identity; `traps` overshooting by exactly `hard_kills` means the two \
         share an increment"
    );
    r.cell
        .metrics_raw()
        .tombstone_invariant()
        .expect("tombstone invariant");

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
    assert_eq!(out.status, 500, "a trap is a 500");
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

    let _ = r.cell.with_instance(killed, |i| i.is_parked());
    assert_eq!(
        r.cell.metrics().tombstone_refusals,
        2,
        "it counts refusals, not tombstones: asking twice is two refusals"
    );

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

/// The four acquisition sources are exhaustive over admitted dispatches, so a
/// job popped twice shows as a sum larger than `dispatches` and a lost one as
/// a caller that never got a reply.
#[test]
fn per_worker_queues_serve_every_dispatch_exactly_once() {
    let mut cfg = CellConfig::for_test("lanes");
    cfg.exec_threads = 8;
    let r = rig(cfg);
    let module = app(&r, "http", FakeBehaviour::done());

    const BATCHES: u64 = 16;
    const EACH: u64 = 8;
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
                .unwrap_or_else(|e| panic!("dispatch {i}/{n} lost: {e:?}"));
            assert_eq!(out.status, 200, "dispatch {i}/{n}: {:?}", out.trap);
        }
    }

    let m = r.cell.metrics();
    assert_eq!(
        m.dispatches,
        BATCHES * EACH,
        "more means a job was popped twice, fewer means one was dropped"
    );
    assert_eq!(
        m.served_warm + m.served_pooled_fresh + m.served_hibernation_wake + m.served_activated,
        m.dispatches,
        "the acquisition sources stay exhaustive over every lane"
    );
    assert_eq!(m.shed, 0, "this load is far inside the admission depth");
    r.cell.shutdown();
}

/// The counter says the drop was seen; `has_ready` says it stranded a real
/// result.
#[test]
fn a_completion_for_an_instance_that_is_gone_is_counted_rather_than_dropped_in_silence() {
    let r = rig(CellConfig::for_test("wake-dropped"));
    // Never resident, so the wake router cannot reach it.
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
        "a completion arrived for an unreachable instance and nothing counted it"
    );
    assert_eq!(m.wakes, 0, "no wake was enqueued, so none may be counted");
    assert!(
        r.cell.futures().has_ready(ghost),
        "the completion was written into the table before the wake was dropped, so the result \
         is stranded"
    );
    r.cell.shutdown();
}

/// The control: a deliverable completion moves `wakes` and not the drop
/// count.
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
    assert_eq!(out.status, 200, "the dispatch was served");

    let m = r.cell.metrics();
    assert_eq!(m.wakes, 1, "the wake was enqueued and must be counted");
    assert_eq!(
        m.wakes_dropped, 0,
        "a live instance's completion was counted as undeliverable"
    );
    r.cell.shutdown();
}
