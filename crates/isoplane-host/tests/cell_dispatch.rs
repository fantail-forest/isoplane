//! Budget-kill isolation, the hang breaker, shedding, and matching results on a shared channel.

use std::sync::Arc;
use std::time::Duration;

use isoplane_test_support::wait_for;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::{DispatchRecord, HttpRequest};
use isoplane_abi::result::ResultRecord;
use isoplane_engine::Completion;

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

/// A host timer that will not fire during any test; parking on it holds an
/// instance Parked for as long as a test needs to look at it.
const PARK_MS: u64 = 600_000;

/// Budget kills release the only worker so another tenant can park and wake.
#[test]
fn a_budget_killed_spinner_does_not_block_another_tenants_wake() {
    let r = rig(CellConfig {
        exec_threads: 1,
        ..CellConfig::for_test("c7")
    });

    let spinner = Arc::new(FakeActivator::named(
        ModuleId::new("spinner", "v1"),
        FakeBehaviour::program([Step::HardKill]),
    ));
    let spin_mod = spinner.module().clone();
    r.cell
        .register_module("noisy", spinner, manifest("spinner", Limits::default()))
        .unwrap();

    // One park-then-wake round trip per `wake_once`; the second continues the
    // program on the instance the first left warm.
    let round_trip = [
        Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        },
        Step::Done {
            status: 200,
            body: b"woken".to_vec(),
        },
    ];
    let victim = Arc::new(FakeActivator::named(
        ModuleId::new("victim", "v1"),
        FakeBehaviour::program(round_trip.iter().cloned().chain(round_trip.iter().cloned())),
    ));
    let victim_mod = victim.module().clone();
    r.cell
        .register_module("quiet", victim, manifest("victim", Limits::default()))
        .unwrap();

    wake_once(&r, &victim_mod);

    let mut spins = Vec::new();
    for i in 0..2 {
        let mut rec = http(&r.cell, &spin_mod, "/spin");
        rec.dispatch_id = 1000 + i;
        rec.occupancy_budget_ms = 50;
        spins.push(
            r.cell
                .submit(
                    &spin_mod,
                    Target::Handler {
                        route: "/spin".into(),
                    },
                    rec,
                )
                .unwrap(),
        );
    }
    for s in spins {
        let out = s.recv_timeout(T).expect("spinner never finished");
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "the spinner was not killed at its budget"
        );
        assert_eq!(out.status, 503, "a kill is a 503");
        let trap = out.trap.expect("a kill names its trap");
        assert!(trap.starts_with("budget_exceeded"), "wrong trap: {trap}");
        assert!(
            trap.contains("without unwinding"),
            "kill was not abrupt: {trap}"
        );
    }

    wake_once(&r, &victim_mod);

    let m = r.cell.metrics();
    assert_eq!(m.tenants["quiet"].traps, 0, "the victim tenant was harmed");
    assert_eq!(
        m.tenants["noisy"].traps, 2,
        "the offender's own dispatches are what suffer"
    );
    assert_eq!(m.budget_kills, 2, "both spinners were killed at budget");
    r.cell.shutdown();
}

/// One park-then-wake round trip for the victim tenant.
fn wake_once(r: &Rig, module: &ModuleId) {
    let before: std::collections::HashSet<u64> =
        r.cell.futures().live_future_ids().into_iter().collect();
    let rec = http(&r.cell, module, "/wake");
    let rx = r
        .cell
        .submit(
            module,
            Target::Handler {
                route: "/wake".into(),
            },
            rec,
        )
        .unwrap();

    // Wait for it to park with a future outstanding. Parking is a *return*
    // (invariant 3), so from here the victim holds no thread at all.
    let id = {
        let mut found = None;
        assert!(
            wait_for(
                || {
                    found = r
                        .cell
                        .futures()
                        .live_future_ids()
                        .into_iter()
                        .find(|i| !before.contains(i));
                    found.is_some()
                },
                Duration::from_secs(5)
            ),
            "the victim never registered a future"
        );
        found.unwrap()
    };

    r.cell.complete_future(id, ResultRecord::Ok(b"v".to_vec()));
    let out = rx.recv_timeout(T).expect("the victim never woke");
    assert_eq!(out.status, 200, "victim failed: {:?}", out.trap);
    assert_eq!(out.slices, 2, "expected dispatch + one wake");
}

#[test]
fn the_breaker_fails_a_repeat_offender_fast_without_entering_the_guest() {
    let r = rig(CellConfig {
        exec_threads: 2,
        ..CellConfig::for_test("breaker")
    });
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("hangs", "v1"),
        FakeBehaviour::program([Step::HardKill]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act.clone(), manifest("hangs", Limits::default()))
        .unwrap();

    let mut killed_occupancy = Vec::new();
    for i in 0..3 {
        let mut rec = http(&r.cell, &module, "/hang");
        rec.dispatch_id = i;
        rec.occupancy_budget_ms = 10;
        let out = r
            .cell
            .dispatch_blocking(
                &module,
                Target::Handler {
                    route: "/hang".into(),
                },
                rec,
                T,
            )
            .unwrap();
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "the spinner was killed"
        );
        killed_occupancy.push(out.occupancy_ns);
    }
    assert!(
        killed_occupancy.iter().all(|c| *c > 5_000_000),
        "every kill cost real occupancy: {killed_occupancy:?}"
    );
    let activations_before = act.activation_count();

    let mut rec = http(&r.cell, &module, "/hang");
    rec.dispatch_id = 99;
    rec.occupancy_budget_ms = 10;
    let out = r
        .cell
        .dispatch_blocking(
            &module,
            Target::Handler {
                route: "/hang".into(),
            },
            rec,
            T,
        )
        .unwrap();
    assert_eq!(out.status, 503, "the breaker answers 503");
    assert_eq!(out.trap.as_deref(), Some("breaker_open"));
    assert_eq!(
        out.occupancy_ns, 0,
        "the breaker burned occupancy to say no"
    );
    assert_eq!(
        act.activation_count(),
        activations_before,
        "the breaker activated an instance to reject the dispatch"
    );
    assert_eq!(
        r.cell.metrics().breaker_rejections,
        1,
        "the rejection is counted"
    );
    r.cell.shutdown();
}

/// One slow dispatch is submitted first and fifty fast ones behind it, so
/// the first result off the shared channel is not the first dispatch in.
#[test]
fn many_dispatches_share_one_channel_and_stay_matchable() {
    let r = rig(CellConfig {
        exec_threads: 4,
        queue_depth_per_worker: 64,
        ..CellConfig::for_test("share")
    });

    let slow = Arc::new(FakeActivator::named(
        ModuleId::new("slow", "v1"),
        FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", 250)],
            },
            Step::Done {
                status: 200,
                body: b"slow".to_vec(),
            },
        ]),
    ));
    let slow_mod = slow.module().clone();
    r.cell
        .register_module("acme", slow, manifest("slow", Limits::default()))
        .unwrap();

    let fast = Arc::new(FakeActivator::named(
        ModuleId::new("fast", "v1"),
        FakeBehaviour::program([Step::Done {
            status: 200,
            body: b"fast".to_vec(),
        }]),
    ));
    let fast_mod = fast.module().clone();
    r.cell
        .register_module("acme", fast, manifest("fast", Limits::default()))
        .unwrap();

    const SLOW_ID: u64 = 9_000;
    const FAST: u64 = 50;
    let (tx, rx) = std::sync::mpsc::sync_channel::<isoplane_host::DispatchResult>(1024);

    let mut rec = http(&r.cell, &slow_mod, "/slow");
    rec.dispatch_id = SLOW_ID;
    assert!(r.cell.submit_with(
        &slow_mod,
        Target::Handler {
            route: "/slow".into()
        },
        rec,
        tx.clone()
    ));

    for i in 0..FAST {
        let mut rec = http(&r.cell, &fast_mod, "/fast");
        rec.dispatch_id = i;
        assert!(r.cell.submit_with(
            &fast_mod,
            Target::Handler {
                route: "/fast".into()
            },
            rec,
            tx.clone()
        ));
    }
    drop(tx);

    let mut ids = Vec::new();
    for _ in 0..=FAST {
        let res = rx.recv_timeout(Duration::from_secs(10)).expect("result");
        assert_eq!(
            res.status, 200,
            "unexpected status for dispatch {}",
            res.dispatch_id
        );
        ids.push(res.dispatch_id);
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "extra result on the channel"
    );

    assert_ne!(
        ids[0], SLOW_ID,
        "the slow dispatch was submitted first; if it also answered first, this test is not          exercising out-of-order completion and proves nothing about correlation"
    );
    assert_eq!(
        *ids.last().unwrap(),
        SLOW_ID,
        "the 250 ms park should answer last"
    );

    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "a dispatch_id came back twice");
    let mut expected: std::collections::HashSet<u64> = (0..FAST).collect();
    expected.insert(SLOW_ID);
    assert_eq!(
        unique, expected,
        "the ids that came back are not the ids that went in"
    );

    r.cell.shutdown();
}

#[test]
fn a_shed_submission_says_so_rather_than_going_quiet() {
    let r = rig(CellConfig {
        exec_threads: 1,
        queue_depth_per_worker: 1,
        ..CellConfig::for_test("shed")
    });
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let act = Arc::new(FakeActivator::named(
        ModuleId::new("spin", "v1"),
        FakeBehaviour::program([Step::Hold {
            entered,
            release: Arc::new(parking_lot::Mutex::new(released)),
        }]),
    ));
    let module = act.module().clone();
    r.cell
        .register_module("acme", act, manifest("spin", Limits::default()))
        .unwrap();

    let (tx, rx) = std::sync::mpsc::sync_channel::<isoplane_host::DispatchResult>(128);
    let mut rec = http(&r.cell, &module, "/spin");
    rec.dispatch_id = 0;
    rec.occupancy_budget_ms = 50;
    assert!(r.cell.submit_with(
        &module,
        Target::Handler {
            route: "/spin".into()
        },
        rec,
        tx.clone()
    ));
    observed
        .recv_timeout(Duration::from_secs(10))
        .expect("worker reached the explicit hold");

    let mut shed = 0;
    let mut accepted = 1;
    for i in 1..64u64 {
        let mut rec = http(&r.cell, &module, "/spin");
        rec.dispatch_id = i;
        rec.occupancy_budget_ms = 50;
        if r.cell.submit_with(
            &module,
            Target::Handler {
                route: "/spin".into(),
            },
            rec,
            tx.clone(),
        ) {
            accepted += 1;
        } else {
            shed += 1;
        }
    }
    drop(tx);
    assert_eq!(
        accepted, 2,
        "one Hot and one queued; everything else must shed at admission"
    );
    assert_eq!(shed, 62, "the rest were shed");
    assert!(
        r.cell.metrics().shed >= 62,
        "the cell must count what it shed"
    );

    drop(release);
    let mut got = 0;
    while rx.recv_timeout(Duration::from_secs(2)).is_ok() {
        got += 1;
    }
    assert_eq!(
        got, accepted,
        "results on the channel must equal accepted submissions"
    );
    r.cell.shutdown();
}

#[test]
fn async_dispatch_completes_while_the_blocking_pool_is_occupied() {
    let r = rig(CellConfig::for_test("async-dispatch"));
    let guest = Arc::new(FakeActivator::named(
        ModuleId::new("async-dispatch", "v1"),
        FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", 0)],
            },
            Step::Done {
                status: 200,
                body: b"resumed".to_vec(),
            },
        ]),
    ));
    let module = guest.module().clone();
    r.cell
        .register_module(
            "tenant",
            guest,
            manifest("async-dispatch", Limits::default()),
        )
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (started, running) = tokio::sync::oneshot::channel();
    let blocker = runtime.spawn_blocking(move || {
        started
            .send(())
            .expect("test must wait for the blocking worker");
        // Dropping the sender also releases this worker if the test fails.
        match wait.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    let result = runtime.block_on(async {
        running.await.expect("blocking worker must start");
        tokio::time::timeout(
            T,
            r.cell.dispatch(
                &module,
                Target::Handler { route: "/".into() },
                http(&r.cell, &module, "/"),
                T,
            ),
        )
        .await
        .expect("async dispatch must not hang behind the occupied blocking pool")
    });
    drop(release);
    runtime
        .block_on(blocker)
        .expect("blocking worker must exit");
    let result = result.expect("dispatch must finish without a blocking worker");
    assert_eq!(
        result.body, b"resumed",
        "a parked dispatch must resume through the async completion channel"
    );
    assert_eq!(
        result.slices, 2,
        "the regression must exercise a park and wake"
    );
    r.cell.shutdown();
}
