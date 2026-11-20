//! Each cell counter is moved by the event it counts. Where a counter is one
//! half of an identity, the identity is asserted too.

use std::sync::Arc;
use std::time::Duration;

use isoplane_test_support::wait_for;

use isoplane_abi::config::{AppManifest, Bindings};
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_abi::status::TrapCode;

use isoplane_harness::fakes::*;
use isoplane_host::cell::{Cell, CellConfig, Services, Target};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::{Activator, ModuleId, Resolver};

const T: Duration = Duration::from_secs(10);

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
            isoplane_harness::uniform_connection_limits(1024),
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
    assert_eq!(
        (out.completion, out.status),
        (isoplane_engine::Completion::BudgetKill, 503),
        "the spin was killed at its budget, which is a 503"
    );

    assert!(
        wait_for(|| r.cell.metrics().tenants["acme"].dispatches == 2, T,),
        "both dispatches must finalize their diagnostic counters"
    );
    let m = r.cell.metrics();
    assert_eq!(
        m.traps, 1,
        "the budget kill was counted as a trap as well as a kill"
    );

    assert_eq!(
        m.tenants["acme"].traps, 1,
        "the tenant trap count also excludes the budget kill"
    );
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
    assert!(
        wait_for(
            || r.cell.slot(out.instance).is_none(),
            Duration::from_secs(10)
        ),
        "a trapped instance is no longer in the cell"
    );
    assert_eq!(
        r.cell.idle_instances(&module),
        Some(0),
        "and nothing of it is kept for reuse"
    );
    r.cell.shutdown();
}
