//! The metrics scrape decomposes the lifecycle, logging is recorded to its dispatch, and the metrics endpoint serves over HTTP.

use std::sync::Arc;
use std::time::Duration;

use isoplane_test_support::wait_for;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};

use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::fakes::*;
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
fn the_metrics_endpoint_decomposes_the_lifecycle() {
    let r = rig(CellConfig::for_test("metrics"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("m", "v1"),
        FakeBehaviour::done(),
    ));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act, manifest("m", Limits::default()))
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
                || r.cell.idle_instances(&module).is_some_and(|idle| idle >= 1),
                Duration::from_secs(5)
            ),
            "the instance never returned to the pool after dispatch {i}"
        );
    }
    let text = r.cell.metrics_text();
    for needle in [
        "isoplane_cell_dispatches 5",
        "isoplane_cell_instances_live",
        "isoplane_cell_instances_parked",
        "isoplane_cell_instances_hibernated",
        "isoplane_cell_activation_p50_us",
        "isoplane_cell_wake_p50_us",
        "isoplane_cell_dirty_pages_p50",
        "isoplane_cell_delta_bytes",
        "isoplane_cell_trap_rate",
        "isoplane_tenant_dispatches{tenant=\"acme\"}",
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
fn logging_is_charged_to_the_dispatch_that_logs() {
    let r = rig(CellConfig::for_test("log-occupancy"));

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
        .register_module("acme", quiet, manifest("quiet-logger", Limits::default()))
        .unwrap();
    r.cell
        .register_module("acme", loud, manifest("loud-logger", Limits::default()))
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

    for (module, result) in [(&qm, &q), (&lm, &l)] {
        assert_eq!(result.status, 204);
        let meters = r.cell.meters(module).expect("tenant meters");
        assert_eq!(meters.occupancy_ns(), result.occupancy_ns);
        assert!(
            meters.log_occupancy_ns() <= meters.occupancy_ns(),
            "logging is a subset of slice occupancy"
        );
    }
    r.cell.shutdown();
}
