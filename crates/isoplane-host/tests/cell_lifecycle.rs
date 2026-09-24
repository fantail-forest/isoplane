//! Fresh vs. reuse residue and entropy isolation across clones.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{HandlerLifecycle, Limits};
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

#[tokio::test]
async fn dropping_a_cell_from_an_async_task_cancels_its_reactor_tasks() {
    let r = rig(CellConfig::for_test("async-drop"));
    let task = r.cell.reactor().spawn(std::future::pending::<()>());
    r.cell.shutdown();
    tokio::spawn(async move { drop(r) })
        .await
        .expect("dropping a cell in an async task must not panic");
    let error = tokio::time::timeout(T, task)
        .await
        .expect("cell shutdown must not leave reactor tasks running")
        .expect_err("cell shutdown must cancel pending reactor tasks");
    assert!(
        error.is_cancelled(),
        "reactor tasks must be cancelled without panic"
    );
}

/// The fake guest's lazy-init probe reports `0` for a pristine instance and
/// `1` for a previous request's residue.
#[test]
fn fresh_leaves_no_residue_reuse_does() {
    let r = rig(CellConfig::for_test("residue"));

    for (app, lifecycle) in [
        ("reuse-app", HandlerLifecycle::Reuse),
        ("fresh-app", HandlerLifecycle::Fresh),
    ] {
        let act = Arc::new(FakeActivator::named(
            ModuleId::new(app, "v1"),
            FakeBehaviour::done().with_lazy_init_probe(),
        ));
        let module = act.module().clone();
        r.cell
            .register_module("acme", act, manifest(app, lifecycle, Limits::default()))
            .unwrap();

        let mut observed = Vec::new();
        for i in 0..25 {
            let mut rec = http(&r.cell, &module, "/probe");
            rec.dispatch_id = i;
            let out = r
                .cell
                .dispatch_blocking(
                    &module,
                    Target::Handler {
                        route: "/probe".into(),
                    },
                    rec,
                    T,
                )
                .unwrap();
            assert_eq!(out.status, 200, "{app} failed: {:?}", out.trap);
            observed.push(out.body[0]);
        }

        match lifecycle {
            HandlerLifecycle::Fresh => {
                assert!(
                    observed.iter().all(|v| *v == 0),
                    "a `fresh` dispatch observed residue: {observed:?}"
                );
                let m = r.cell.meters(&module).unwrap();
                assert_eq!(m.dispatches(), 25, "every dispatch is counted");
            }
            HandlerLifecycle::Reuse => {
                assert_eq!(observed[0], 0, "the first dispatch cannot observe residue");
                assert!(
                    observed[1..].contains(&1),
                    "`reuse` must let per-instance memoization pay off: {observed:?}"
                );
                let m = r.cell.meters(&module).unwrap();
            }
        }
    }
    r.cell.shutdown();
}

#[test]
fn clones_of_one_template_never_share_entropy() {
    let r = rig(CellConfig::for_test("entropy"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("rng", "v1"),
        FakeBehaviour::program([Step::Entropy { bytes: 32 }]),
    ));
    let module = act.module().clone();
    // `fresh`: every dispatch is a clone of the same template.
    r.cell
        .register_module(
            "acme",
            act.clone(),
            manifest("rng", HandlerLifecycle::Fresh, Limits::default()),
        )
        .unwrap();

    for i in 0..50 {
        let mut rec = http(&r.cell, &module, "/rng");
        rec.dispatch_id = i;
        r.cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/rng".into(),
                },
                rec,
                T,
            )
            .unwrap();
    }
    let draws = act.entropy.lock().clone();
    assert_eq!(draws.len(), 50, "every dispatch drew entropy");
    let unique: std::collections::HashSet<Vec<u8>> = draws.into_iter().collect();
    assert_eq!(
        unique.len(),
        50,
        "clones of one template shared an RNG stream"
    );
    assert!(
        unique.iter().all(|d| d.iter().any(|b| *b != 0)),
        "entropy was zero-filled"
    );
    r.cell.shutdown();
}

#[test]
fn a_retired_module_refuses_new_work_and_discards_its_idle_instances() {
    let r = rig(CellConfig::for_test("retire"));
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("retired-app", "v1"),
        FakeBehaviour::done(),
    ));
    let module = act.module().clone();
    r.cell
        .register_module(
            "acme",
            act,
            manifest("retired-app", HandlerLifecycle::Reuse, Limits::default()),
        )
        .unwrap();
    let handler = || Target::Handler { route: "/".into() };
    let served = r
        .cell
        .dispatch_blocking(&module, handler(), http(&r.cell, &module, "/"), T)
        .unwrap();
    assert_eq!(
        served.status, 200,
        "the module must serve before it retires"
    );
    let deadline = std::time::Instant::now() + T;
    while r.cell.idle_instances(&module) != Some((1, 0)) {
        assert!(
            std::time::Instant::now() < deadline,
            "the served instance must return to the pool"
        );
        std::thread::yield_now();
    }
    assert!(r.cell.retire_module(&module), "the module was registered");
    assert_eq!(
        r.cell.metrics().live,
        0,
        "retiring a module must discard its idle instances"
    );
    assert!(
        r.cell
            .dispatch_blocking(&module, handler(), http(&r.cell, &module, "/"), T)
            .is_err(),
        "a retired module must not admit new work"
    );
    r.cell.shutdown();
}
