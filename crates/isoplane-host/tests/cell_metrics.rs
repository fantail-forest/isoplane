//! What a cell counts for the dispatches it serves: activations, and each
//! application's occupied time.

use std::sync::Arc;
use std::time::Duration;

use isoplane_test_support::wait_for;

use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};

use isoplane_harness::fakes::*;
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

#[test]
fn sequential_dispatches_are_counted_and_pay_for_one_activation() {
    let r = rig(isoplane_harness::fakes::cell_config("metrics"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("m", "v1"),
        FakeBehaviour::done(),
    ));
    let module = act.module().clone();
    r.cell.register_module("acme", act, manifest("m")).unwrap();
    let mut acquired = Vec::new();
    for i in 0..5 {
        let mut rec = http(&r.cell, &module, "/");
        rec.dispatch_id = i;
        let result = r
            .cell
            .dispatch_blocking(&module, Target::Handler { route: "/".into() }, rec, T)
            .unwrap();
        acquired.push(result.acquired);
        // The response leaves before the instance is pooled, so a client that
        // fires the next request at once can race the release and pay for a
        // second activation; waiting for the pool measures the steady state.
        assert!(
            wait_for(
                || r.cell.idle_instances(&module).is_some_and(|idle| idle >= 1),
                Duration::from_secs(5)
            ),
            "the instance never returned to the pool after dispatch {i}"
        );
    }
    assert_eq!(
        acquired,
        ["activated", "warm", "warm", "warm", "warm"],
        "only the first request paid for an activation"
    );
    assert_eq!(
        (
            r.cell.metrics().dispatches,
            r.cell.metrics_raw().activation_us.count()
        ),
        (5, 1),
        "the cell counts every dispatch and the one activation"
    );
    r.cell.shutdown();
}

#[test]
fn each_application_is_charged_for_its_own_dispatches() {
    let r = rig(isoplane_harness::fakes::cell_config("occupancy"));
    let app = |name: &str, slice_occupancy_ns: u64| {
        let mut behaviour = FakeBehaviour::done();
        behaviour.slice_occupancy = Some(Duration::from_nanos(slice_occupancy_ns));
        let activator = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
        let module = activator.module().clone();
        r.cell
            .register_module("acme", activator, manifest(name))
            .unwrap();
        module
    };
    let (brief, long) = (app("brief", 7), app("long", 13));

    for (module, dispatches) in [(&brief, 2), (&long, 3)] {
        for _ in 0..dispatches {
            let result = r
                .cell
                .dispatch_blocking(
                    module,
                    Target::Handler { route: "/".into() },
                    http(&r.cell, module, "/"),
                    T,
                )
                .unwrap();
            assert_eq!(result.status, 200);
        }
    }

    let occupancy_ns = |module| r.cell.meters(module).expect("module meters").occupancy_ns();
    assert_eq!(
        (occupancy_ns(&brief), occupancy_ns(&long)),
        (2 * 7, 3 * 13),
        "each application's meter holds the occupancy of its own slices"
    );
    r.cell.shutdown();
}
