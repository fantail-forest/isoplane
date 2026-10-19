//! Each cell counter is moved by the event it counts. Where a counter is one
//! half of an identity, the identity is asserted too.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{AppManifest, Bindings, Limits};
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_abi::status::TrapCode;

use isoplane_host::cell::{Cell, CellConfig, Services, Target};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fakes::*;
use isoplane_host::{Activator, ModuleId, Resolver};

const T: Duration = Duration::from_secs(10);
/// A host timer that never fires during a test.
const PARK_MS: u64 = 600_000;

struct Fixed(Vec<std::net::SocketAddr>);
#[async_trait::async_trait]
impl Resolver for Fixed {
    async fn resolve(
        &self,
        _h: &str,
        _p: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        Ok(self.0.clone())
    }
}

struct Rig {
    cell: Arc<Cell>,
}

fn rig(cfg: CellConfig) -> Rig {
    let fetcher = Arc::new(FakeFetcher::default());
    let svc = Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])),
            isoplane_host::egress::ConnectionLimits::uniform(1024).expect("connection limits"),
        )),
        fetcher: fetcher.clone(),
    });
    Rig {
        cell: Cell::new(cfg, svc).expect("a cell starts"),
    }
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        guest_memory_bytes: None,
        resource_limits: Default::default(),
        validation_limits: Default::default(),
        name: name.into(),
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
    r.cell.register_module("acme", act, manifest(name)).unwrap();
    module
}

/// A kill and a trap each end an instance, and are counted apart.
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
    let spinner = app(&r, "spinner", FakeBehaviour::program([Step::HardKill]));

    let mut rec = http(&r.cell, &trapper, "/trap");
    rec.occupancy_budget_ms = 50;
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
    rec.occupancy_budget_ms = 50;
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
    assert_eq!(m.budget_kills, 1, "the spin was killed");
    assert_eq!(
        m.traps, 1,
        "the budget kill was counted as a trap as well as a kill"
    );

    assert_eq!(m.tenants["acme"].traps, 2, "both are faults for the tenant");
    r.cell.shutdown();
}

#[test]
fn a_trapped_instance_is_evicted() {
    let r = rig(CellConfig::for_test("trap-eviction"));
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
    // The response precedes the instance's release.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while r.cell.slot(out.instance).is_some() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        r.cell.slot(out.instance).is_none(),
        "a trapped instance is no longer in the cell"
    );
    assert_eq!(
        r.cell.idle_instances(&module),
        Some(0),
        "and nothing of it is kept for reuse"
    );
    r.cell.shutdown();
}

/// The counter says the drop was seen; `has_ready` says it stranded a real
/// result.
#[test]
fn a_completion_for_an_instance_that_is_gone_is_counted_rather_than_dropped_in_silence() {
    let r = rig(CellConfig::for_test("wake-dropped"));
    // Never resident, so the wake router cannot reach it.
    let ghost = isoplane_host::InstanceId::alloc();
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
        .complete_future(fid, isoplane_abi::result::ResultRecord::Ok(vec![]));

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
        .complete_future(fid, isoplane_abi::result::ResultRecord::Ok(vec![]));
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
