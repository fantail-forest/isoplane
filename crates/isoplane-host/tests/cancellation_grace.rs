//! The grace a dispatch has to finish once its cancellation is requested,
//! through the cell with a scripted guest.

mod common;

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Target};
use isoplane_host::fakes::{FakeActivator, FakeBehaviour, Step};
use isoplane_host::slot::LifecycleTier;
use isoplane_host::{Activator, InstanceId, ModuleId};

const GRACE_MS: u64 = 200;

/// Long enough that a dispatch waiting on it is still waiting when the test
/// looks.
const WAIT_MS: u64 = 60_000;

fn rig(program: Vec<Step>) -> (Arc<Cell>, ModuleId) {
    rig_with_grace(program, GRACE_MS)
}

fn rig_with_grace(program: Vec<Step>, grace_ms: u64) -> (Arc<Cell>, ModuleId) {
    let (cell, _) = common::rig_resolving(
        CellConfig {
            cancellation_grace_ms: grace_ms,
            ..CellConfig::for_test("cancellation-grace")
        },
        "93.184.216.34:80",
    );
    let activator = Arc::new(FakeActivator::new(FakeBehaviour::program(program)));
    let module = activator.module().clone();
    cell.register_module(
        "acme",
        activator,
        common::manifest("grace", Limits::default()),
    )
    .expect("register the scripted guest");
    (cell, module)
}

fn submit(cell: &Cell, module: &ModuleId) -> Receiver<DispatchResult> {
    let record = cell.http_record(
        module,
        HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    );
    cell.submit(module, Target::Handler { route: "/".into() }, record)
        .expect("the cell must admit the dispatch")
}

fn result(dispatch: &Receiver<DispatchResult>) -> DispatchResult {
    dispatch
        .recv_timeout(Duration::from_secs(30))
        .expect("the dispatch must be answered")
}

/// Waits, within a deadline that fails a hung test, until the cell's only
/// instance has run a slice and parked, and returns it.
fn parked(cell: &Cell) -> InstanceId {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let [id] = cell.resident_ids()[..] {
            if cell
                .slot(id)
                .is_some_and(|slot| slot.parks() > 0 && slot.state() == LifecycleTier::Parked)
            {
                return id;
            }
        }
        assert!(Instant::now() < deadline, "the instance never parked");
        std::thread::yield_now();
    }
}

#[test]
fn a_dispatch_still_waiting_when_its_grace_runs_out_is_killed_with_its_instance() {
    let (cell, module) = rig(vec![
        // The first dispatch waits on a timer.
        Step::Park {
            imports: vec![("clock_deadline_in", WAIT_MS)],
        },
        // The second runs out of budget, then ignores the cancellation and
        // waits on nothing at all.
        Step::EnterGrace,
        Step::Park { imports: vec![] },
    ]);
    let waiting = submit(&cell, &module);
    let instance = parked(&cell);
    let ignoring = submit(&cell, &module);

    let killed = result(&ignoring);
    assert_eq!(
        (killed.status, killed.trap.as_deref()),
        (503, Some("cancellation_grace_expired")),
        "the dispatch that outlived its grace must fail for it"
    );
    let discarded = result(&waiting);
    assert_eq!(
        (discarded.status, discarded.trap.as_deref()),
        (503, Some("instance_discarded")),
        "the other dispatch on the instance must fail with it"
    );
    assert_eq!(
        killed.instance, instance,
        "both dispatches ran on one instance"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while cell.slot(instance).is_some() {
        assert!(
            Instant::now() < deadline,
            "the killed dispatch's instance must be evicted"
        );
        std::thread::yield_now();
    }
    assert_eq!(cell.metrics().cancellation_grace_kills, 1);
    cell.shutdown();
}

#[test]
fn a_dispatch_that_finishes_inside_its_grace_keeps_its_instance() {
    let (cell, module) = rig(vec![
        Step::EnterGrace,
        Step::Done {
            status: 200,
            body: b"stopped".to_vec(),
        },
    ]);
    let first = result(&submit(&cell, &module));
    assert_eq!(
        first.status, 200,
        "the dispatch finished after cancellation"
    );

    // The grace runs out after the dispatch has finished.
    std::thread::sleep(Duration::from_millis(GRACE_MS * 4));
    let next = result(&submit(&cell, &module));
    assert_eq!(
        (next.status, next.instance, next.acquired),
        (200, first.instance, "warm"),
        "the instance must outlive a grace its dispatch finished inside"
    );
    assert_eq!(cell.metrics().cancellation_grace_kills, 0);
    let metrics = cell.metrics();
    assert_eq!(
        (metrics.instance_holds, metrics.activations_for_holds),
        (1, 0),
        "the grace held the instance until its dispatch finished"
    );
    cell.shutdown();
}

#[test]
fn a_dispatch_goes_to_another_instance_while_one_in_its_grace_holds_the_first() {
    // Long enough that nothing is killed while the test looks.
    let (cell, module) = rig_with_grace(
        vec![Step::EnterGrace, Step::Park { imports: vec![] }],
        WAIT_MS,
    );
    let _ignoring = submit(&cell, &module);
    let deadline = Instant::now() + Duration::from_secs(30);
    while cell.metrics().instance_holds == 0 {
        assert!(
            Instant::now() < deadline,
            "the dispatch never entered its grace"
        );
        std::thread::yield_now();
    }
    let held = cell.resident_ids()[0];
    let _next = submit(&cell, &module);
    while cell.metrics().served_activated < 2 {
        assert!(
            Instant::now() < deadline,
            "the next dispatch never got an instance"
        );
        std::thread::yield_now();
    }
    let metrics = cell.metrics();
    assert_eq!(
        (metrics.served_shared, metrics.activations_for_holds),
        (0, 1),
        "the next dispatch must not join the held instance"
    );
    assert_eq!(
        cell.resident_ids()
            .into_iter()
            .filter(|id| *id != held)
            .count(),
        1,
        "the next dispatch runs on a second instance"
    );
    let meters = cell.meters(&module).expect("module meters");
    assert_eq!(
        (meters.instance_holds(), meters.activations_for_holds()),
        (1, 1)
    );
    cell.shutdown();
}
