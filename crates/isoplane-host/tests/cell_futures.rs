//! Request future limits and response-completion handover.

use std::sync::Arc;
use std::time::Duration;

use isoplane_test_support::wait_for;

use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_engine::Completion;

use isoplane_harness::fakes::*;
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::{Activator, ModuleId};

mod common;
use common::manifest;

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

const T: Duration = Duration::from_secs(10);

/// A host timer that will not fire during any test; parking on it holds an
/// instance Parked for as long as a test needs to look at it.
const PARK_MS: u64 = 600_000;

#[test]
fn the_pending_futures_cap_refuses_the_extra_operation_and_keeps_the_instance_serving() {
    let r = rig(CellConfig::for_test("independent-app-limits"));
    let limit = 6;

    let act = Arc::new(FakeActivator::named(
        ModuleId::new("greedy", "v1"),
        FakeBehaviour::program([
            Step::Park {
                imports: (0..limit + 3)
                    .map(|_| ("clock_deadline_in", PARK_MS))
                    .collect(),
            },
            Step::Done {
                status: 200,
                body: b"still serving".to_vec(),
            },
        ]),
    ));
    let module = act.module().clone();
    let mut bounded = manifest("greedy");
    bounded.resource_limits.pending_futures_per_dispatch = Some(limit);
    r.cell.register_module("acme", act, bounded).unwrap();

    let mut rec = http(&r.cell, &module, "/greedy");
    rec.dispatch_id = 1;
    assert_eq!(
        rec.pending_futures_limit, limit,
        "the record tells the guest the cap the host applies"
    );
    let greedy = r
        .cell
        .submit(
            &module,
            Target::Handler {
                route: "/greedy".into(),
            },
            rec,
        )
        .unwrap();
    assert!(
        wait_for(|| r.cell.metrics().pending_cap_rejections == 1, T),
        "the operation past the cap is refused: {:?}",
        r.cell.metrics()
    );
    let instance = r.cell.resident_ids()[0];
    assert_eq!(
        r.cell
            .futures()
            .pending_for_import(instance, "clock_deadline_in"),
        limit as usize,
        "the operations within the cap stay pending"
    );
    assert!(
        matches!(greedy.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "the capped dispatch keeps waiting on the operations it was allowed"
    );

    let mut rec = http(&r.cell, &module, "/again");
    rec.dispatch_id = 2;
    let again = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/again".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(
        (
            again.instance,
            again.completion,
            again.status,
            again.body.as_slice()
        ),
        (instance, Completion::Done, 200, b"still serving".as_slice()),
        "the instance the cap refused on still serves its next dispatch"
    );
    assert_eq!(r.cell.metrics().traps, 0, "a cap refusal is not a trap");

    let ok = Arc::new(FakeActivator::named(
        ModuleId::new("higher-limit-app", "v1"),
        FakeBehaviour::program([Step::Park {
            imports: (0..limit + 3)
                .map(|_| ("clock_deadline_in", PARK_MS))
                .collect(),
        }]),
    ));
    let ok_mod = ok.module().clone();
    r.cell
        .register_module("acme", ok, manifest("higher-limit-app"))
        .unwrap();
    let mut rec = http(&r.cell, &ok_mod, "/ok");
    rec.dispatch_id = 2;
    let rx = r
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
            || r.cell.futures().live_futures() >= (limit + 3) as usize,
            Duration::from_secs(5)
        ),
        "the sibling application was capped by another application's allowance"
    );
    assert_eq!(
        r.cell.metrics().pending_cap_rejections,
        1,
        "only the application with the small allowance was capped"
    );
    drop(rx);
    drop(greedy);
    r.cell.shutdown();
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
        .register_module("acme", act, manifest("handover"))
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
        .register_module("tenant", activator, manifest("cancellation"))
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
    let mut app = manifest("bounded");
    app.resource_limits.pending_futures_per_dispatch = Some(2);
    app.resource_limits.occupancy_ms_per_dispatch = Some(1234);
    r.cell
        .register_module("acme", act, app)
        .expect("configured module registers");
    let rec = http(&r.cell, &module, "/bounded");
    assert_eq!(
        (rec.occupancy_budget_ms, rec.pending_futures_limit),
        (1234, 2),
        "the dispatch must carry the configured occupancy budget and pending cap"
    );
    let parked = r
        .cell
        .submit(
            &module,
            Target::Handler {
                route: "/bounded".into(),
            },
            rec,
        )
        .expect("the cell admits the dispatch");
    assert!(
        wait_for(|| r.cell.metrics().pending_cap_rejections == 1, T),
        "the third pending future must trip the operator cap: {:?}",
        r.cell.metrics()
    );
    assert_eq!(
        r.cell
            .futures()
            .pending_for_import(r.cell.resident_ids()[0], "clock_deadline_in"),
        2,
        "the first two stay pending"
    );
    assert_eq!(
        r.cell.metrics().traps,
        0,
        "the configured cap refuses an operation without trapping"
    );
    drop(parked);
    r.cell.shutdown();
}
