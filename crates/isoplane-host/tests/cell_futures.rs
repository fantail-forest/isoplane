//! Request future limits and response-completion handover.

use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_engine::Completion;

use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::fakes::*;
use isoplane_host::{Activator, ModuleId};

mod common;
use common::manifest;
use isoplane_harness::EmittedGuestLogs as _;

struct Rig {
    cell: Arc<Cell>,
}

fn rig(cfg: CellConfig) -> Rig {
    let (cell, _fetcher) = common::rig_resolving(cfg, "93.184.216.34:80");
    Rig { cell }
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

#[test]
fn the_pending_futures_cap_traps_the_guest() {
    let free = rig(CellConfig {
        ..CellConfig::for_test("cap-free")
    });
    let paid = rig(CellConfig::for_test("cap-paid"));
    let r = &free;
    let limit = Limits {
        instances_per_application: 3,
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
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
                Limits {
                    instances_per_application: 3,
                    module_bytes_compressed: 5242880,
                    package_bytes: 52428800,
                    instance_memory_max: 134217728,
                    occupancy_ms_per_dispatch: 100,
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
        .register_module("acme", ok, manifest("higher-limit-app", Limits::default()))
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
fn response_completion_hands_work_to_background_without_holding_the_request_open() {
    isoplane_harness::LogCapture::global();
    let r = rig(CellConfig::for_test("handover"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("handover", "v1"),
        FakeBehaviour::program([
            Step::RespondLeaving {
                imports: vec![("clock_deadline_in", PARK_MS)],
            },
            Step::Log { bytes: 1, times: 1 },
            Step::Done {
                status: 200,
                body: b"warm".to_vec(),
            },
        ]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act, manifest("handover", Limits::default()))
        .unwrap();
    let send = || {
        r.cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                http(&r.cell, &module, "/"),
                T,
            )
            .unwrap()
    };
    let first = send();
    assert_eq!(
        (first.status, first.body.as_slice()),
        (200, b"responded".as_slice())
    );
    assert!(
        wait_for(
            || r.cell.metrics_raw().dispatch_us.count() == 1
                && r.cell.idle_instances(&module) == Some(1),
            T
        ),
        "the request ends while its operation is still pending"
    );
    assert_eq!(r.cell.metrics().work_handed_to_instance, 1);
    assert_eq!(r.cell.futures().live_futures(), 1);
    let future = r.cell.futures().live_future_ids()[0];
    r.cell
        .complete_future(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
    assert!(
        wait_for(
            || r.cell.metrics_raw().dispatch_us.count() == 2
                && r.cell.idle_instances(&module) == Some(1),
            T
        ),
        "background work runs without a new request"
    );
    let slot = r
        .cell
        .slot(first.instance)
        .expect("the instance remains warm");
    assert_eq!(
        slot.ctx.emitted_logs()[0].dispatch,
        isoplane_host::futures::INSTANCE_WORK
    );
    assert_eq!(
        r.cell.meters(&module).unwrap().dispatches(),
        1,
        "background execution is not a request"
    );
    let second = send();
    assert_eq!(
        (second.instance, second.status, second.body.as_slice()),
        (first.instance, 200, b"warm".as_slice())
    );
    r.cell.shutdown();
}

#[test]
fn entering_grace_wakes_the_dispatch_cancellation_future() {
    let rig = rig(CellConfig::for_test("cancellation"));
    let activator = Arc::new(FakeActivator::named(
        ModuleId::new("cancellation", "v1"),
        FakeBehaviour::program([
            Step::EnterGrace,
            Step::Done {
                status: 200,
                body: b"cancelled".to_vec(),
            },
        ]),
    ));
    let module = activator.module().clone();
    rig.cell
        .register_module(
            "tenant",
            activator,
            manifest("cancellation", Limits::default()),
        )
        .expect("registering the cancellation guest");
    let result = rig
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler { route: "/".into() },
            http(&rig.cell, &module, "/"),
            T,
        )
        .expect("the cancellation wake must resume the parked guest");
    assert_eq!(
        result.body, b"cancelled",
        "entering grace must preserve and complete the dispatch cancellation future"
    );
}

#[test]
fn operator_budgets_reach_the_dispatch_and_pending_future_table() {
    let r = rig(CellConfig::for_test("operator-budgets"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("bounded", "v1"),
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS); 3],
        }]),
    ));
    let module = act.module().clone();
    let mut app = manifest("bounded", Limits::default());
    app.resource_limits.pending_futures_per_dispatch = Some(2);
    app.resource_limits.occupancy_ms_per_dispatch = Some(1234);
    r.cell
        .register_module("acme", act, app)
        .expect("configured module registers");
    let rec = http(&r.cell, &module, "/bounded");
    assert_eq!(
        rec.occupancy_budget_ms, 1234,
        "dispatch must carry the configured occupancy budget"
    );
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/bounded".into(),
            },
            rec,
            T,
        )
        .expect("the cell contains the resource refusal");
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "the third pending future must trip the operator cap"
    );
    assert_eq!(
        r.cell.metrics().pending_cap_rejections,
        1,
        "the configured cap refusal must be observable"
    );
}
