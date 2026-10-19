//! Several dispatches in progress on one instance, through the real engine and
//! cell with `guests/quotes-api`, whose `/park?ms=` waits on a host timer.

mod common;

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, IsoplaneEngine};
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Target};
use isoplane_host::slot::LifecycleTier;
use isoplane_host::{EngineActivator, InstanceId, ModuleId};

/// Long enough that a waiting dispatch is still waiting when the test checks.
const WAIT_MS: &str = "30000";

fn rig() -> (Arc<Cell>, ModuleId) {
    let engine =
        Arc::new(IsoplaneEngine::new(isoplane_fixtures::engine_config(16)).expect("engine"));
    let module = ModuleId::new("quotes-api", "v1");
    let activator = EngineActivator::load(
        engine,
        module.clone(),
        &isoplane_fixtures::rust("quotes-api"),
    )
    .expect("quotes-api.wasm must compile");
    let (cell, _) = common::rig_resolving(CellConfig::for_test("interleaving"), "93.184.216.34:80");
    cell.register_module(
        "acme",
        Arc::new(activator),
        common::manifest("quotes-api", Limits::default()),
    )
    .expect("register quotes-api");
    (cell, module)
}

fn request(
    cell: &Cell,
    module: &ModuleId,
    path: &str,
    query: Vec<(String, String)>,
) -> isoplane_abi::dispatch::DispatchRecord {
    cell.http_record(
        module,
        HttpRequest {
            method: "GET".into(),
            url: format!("http://app.example{path}"),
            path: path.into(),
            query,
            headers: vec![],
            body: vec![],
        },
    )
}

/// Starts a dispatch that waits on a host timer for [`WAIT_MS`].
fn waiting(cell: &Cell, module: &ModuleId) -> Receiver<DispatchResult> {
    let record = request(cell, module, "/park", vec![("ms".into(), WAIT_MS.into())]);
    cell.submit(
        module,
        Target::Handler {
            route: "/park".into(),
        },
        record,
    )
    .expect("the cell must admit the waiting dispatch")
}

/// Waits, within a deadline that fails a hung test, until `admitted`
/// dispatches have their instances and every resident instance has run a
/// slice and parked.
fn until_parked(cell: &Cell, admitted: u64) -> Vec<InstanceId> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let metrics = cell.metrics();
        let ids = cell.resident_ids();
        let parked = ids.iter().all(|id| {
            cell.slot(*id)
                .is_some_and(|slot| slot.parks() > 0 && slot.state() == LifecycleTier::Parked)
        });
        if metrics.served_activated + metrics.served_shared == admitted && parked {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "the cell never parked {admitted} dispatches: {ids:?}"
        );
        std::thread::yield_now();
    }
}

#[test]
fn a_dispatch_joins_an_instance_whose_dispatch_is_waiting_and_finishes_first() {
    let (cell, module) = rig();
    let first = waiting(&cell, &module);
    let instance = until_parked(&cell, 1)[0];

    let record = request(&cell, &module, "/routes", vec![]);
    let second = cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/routes".into(),
            },
            record,
            Duration::from_secs(30),
        )
        .expect("the second dispatch must finish");
    assert_eq!(
        (second.status, second.completion),
        (200, Completion::Done),
        "the second dispatch must succeed: {second:?}"
    );
    assert_eq!(
        second.instance, instance,
        "the second dispatch must run on the instance the first is waiting on"
    );
    assert!(
        matches!(first.try_recv(), Err(TryRecvError::Empty)),
        "the first dispatch must still be waiting when the second finishes"
    );
    assert_eq!(
        cell.metrics().served_shared,
        1,
        "the second dispatch joined, it did not activate"
    );
    cell.shutdown();
}

#[test]
fn a_full_instance_sends_the_next_dispatch_to_another() {
    let (cell, module) = rig();
    let mut held = Vec::new();
    for admitted in 1..=8 {
        held.push(waiting(&cell, &module));
        until_parked(&cell, admitted);
    }
    held.push(waiting(&cell, &module));
    let instances = until_parked(&cell, 9);
    assert_eq!(
        (instances.len(), cell.metrics().served_shared),
        (2, 7),
        "the ninth dispatch must get a second instance once the first holds eight"
    );
    for dispatch in &held {
        assert!(
            matches!(dispatch.try_recv(), Err(TryRecvError::Empty)),
            "every dispatch must still be waiting"
        );
    }
    cell.shutdown();
}
