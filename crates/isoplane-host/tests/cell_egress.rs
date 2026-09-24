//! A guest cannot reach a private or link-local address through `http_fetch`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_abi::result::{FutureError, ResultRecord};

use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::fakes::*;
use isoplane_host::{Activator, ModuleId};

mod common;
use common::manifest;

struct Rig {
    cell: Arc<Cell>,
    fetcher: Arc<FakeFetcher>,
}

fn rig_resolving(cfg: CellConfig, addr: &str) -> Rig {
    let (cell, fetcher) = common::rig_resolving(cfg, addr);
    Rig { cell, fetcher }
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

#[test]
fn a_guest_cannot_fetch_a_private_address() {
    let r = rig_resolving(CellConfig::for_test("ssrf"), "169.254.169.254:80");
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("prober", "v1"),
        FakeBehaviour::program([Step::Fetch {
            url: "http://169.254.169.254/latest/meta-data/".into(),
            times: 1,
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act.clone(),
            manifest("prober", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    let rx = r
        .cell
        .submit(
            &module,
            Target::Handler {
                route: "/probe".into(),
            },
            http(&r.cell, &module, "/probe"),
        )
        .unwrap();
    assert!(
        wait_for(|| !act.wakes().is_empty(), Duration::from_secs(5)),
        "the guest was never told what happened to its fetch"
    );
    let (_, res) = act.wakes()[0].clone();
    match res {
        ResultRecord::Err(FutureError::Denied(m)) => {
            assert!(m.contains("169.254.169.254"), "{m}");
        }
        other => panic!("the metadata endpoint was reachable: {other:?}"),
    }
    assert!(
        r.fetcher.connected_to.lock().is_empty(),
        "the host dialled a link-local address"
    );
    drop(rx);
    r.cell.shutdown();
}

#[test]
fn an_endpoint_exception_lets_only_its_own_application_reach_a_private_peer() {
    let r = rig_resolving(CellConfig::for_test("exceptions"), "10.0.0.5:80");
    let fetching = |name: &str| {
        Arc::new(FakeActivator::named(
            ModuleId::new(name, "v1"),
            FakeBehaviour::program([Step::Fetch {
                url: "http://10.0.0.5/".into(),
                times: 1,
            }]),
        ))
    };
    let excepted = fetching("database-client");
    let other = fetching("neighbour");
    r.cell
        .register_module_with_exceptions(
            "acme",
            excepted.clone(),
            manifest(
                "database-client",
                HandlerLifecycle::Fresh,
                Limits::default(),
            ),
            Arc::new(["10.0.0.5:80".parse().expect("endpoint")]),
        )
        .unwrap();
    r.cell
        .register_module(
            "acme",
            other.clone(),
            manifest("neighbour", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();
    let mut pending = Vec::new();
    for act in [&excepted, &other] {
        let module = act.module().clone();
        pending.push(
            r.cell
                .submit(
                    &module,
                    Target::Handler {
                        route: "/query".into(),
                    },
                    http(&r.cell, &module, "/query"),
                )
                .unwrap(),
        );
        assert!(
            wait_for(|| !act.wakes().is_empty(), Duration::from_secs(5)),
            "the guest was never told what happened to its fetch"
        );
    }
    assert!(
        !matches!(
            excepted.wakes()[0].1,
            ResultRecord::Err(FutureError::Denied(_))
        ),
        "the application granted the exception must reach its private peer: {:?}",
        excepted.wakes()[0].1
    );
    assert!(
        matches!(
            other.wakes()[0].1,
            ResultRecord::Err(FutureError::Denied(_))
        ),
        "another application on the same cell must not inherit the exception: {:?}",
        other.wakes()[0].1
    );
    drop(pending);
    r.cell.shutdown();
}
