//! Raw debugger guests retain shared cell dispatch, wake, and terminal cleanup.

mod common;

use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_abi::result::ResultRecord;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::debug::{DebugActivator, DEBUG_BUDGET_MS};
use isoplane_host::{Cell, CellConfig, LifecycleTier, ModuleId, Target};
use std::sync::Arc;
use std::time::Duration;

struct Rig {
    cell: Arc<Cell>,
    module: ModuleId,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.cell.shutdown();
    }
}

fn rig(park: bool) -> Rig {
    let handler = if park {
        "i64.const 600000 call $timer drop i64.const 1"
    } else {
        "i64.const 0"
    };
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (global $initialized (mut i32) (i32.const 0))
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
        (func (export "isoplane_initialize") (result i64) i32.const 42 global.set $initialized i64.const 0)
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            global.get $initialized i32.const 42 i32.ne if unreachable end {handler})
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_gc_hint") (param i32) unreachable))"#
    ))
    .expect("the raw cell fixture must assemble");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 4,
            max_memory_bytes: 2 << 20,
            guard_size_bytes: 65536,
            ..Default::default()
        })
        .expect("the real engine must start"),
    );
    let module = ModuleId::new("debug-cell", "v1");
    let activator = Arc::new(
        DebugActivator::load(engine, module.clone(), &wasm)
            .expect("the raw debug fixture must load"),
    );
    let (cell, _) = common::rig_resolving(
        CellConfig {
            wall_ms_cap: DEBUG_BUDGET_MS,
            ..CellConfig::for_test("debug-cell")
        },
        "93.184.216.34:80",
    );
    cell.register_debug_module(
        "owner",
        activator.clone(),
        common::manifest("debug-cell", HandlerLifecycle::Reuse, Limits::default()),
    )
    .expect("debug registration must establish fresh execution");
    Rig { cell, module }
}

fn request() -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url: "http://debug.test/".into(),
        path: "/".into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    }
}

#[tokio::test]
async fn a_debug_request_initializes_raw_state_and_is_discarded_after_completion() {
    let rig = rig(false);
    let record = rig.cell.http_record(&rig.module, request());
    assert_eq!(
        record.cpu_budget_ms, DEBUG_BUDGET_MS,
        "debugger pauses must use the explicit local execution backstop"
    );
    let result = rig
        .cell
        .dispatch_blocking(
            &rig.module,
            Target::Handler { route: "/".into() },
            record,
            Duration::from_secs(30),
        )
        .expect("the shared cell must complete the debug request");
    assert_eq!(
        result.completion,
        Completion::Done,
        "raw initialization must precede the handler"
    );
    assert_eq!(result.status, 204, "the fixture has no response body");
    await_terminal_cleanup(&rig.cell, result.instance).await;
    assert!(
        rig.cell.resident_ids().is_empty(),
        "debug state must not enter an idle reuse pool"
    );
    assert!(
        rig.cell.futures().live_guest_future_ids().is_empty(),
        "completion must release every debug request future"
    );
}

#[tokio::test]
async fn an_awaiting_debug_guest_refuses_capture_and_wakes_through_the_cell() {
    let rig = rig(true);
    let receiver = rig
        .cell
        .submit(
            &rig.module,
            Target::Handler { route: "/".into() },
            rig.cell.http_record(&rig.module, request()),
        )
        .expect("the debug request must be admitted");
    let (instance, future) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(instance) = rig.cell.resident_ids().first().copied() {
                if rig.cell.instance_state(instance) == Some(LifecycleTier::Parked) {
                    if let Some(future) =
                        rig.cell.futures().live_guest_future_ids().first().copied()
                    {
                        break (instance, future);
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the debug request never parked on its real host timer");
    let cell = rig.cell.clone();
    let capture = tokio::task::spawn_blocking(move || cell.force_hibernate(instance))
        .await
        .expect("capture refusal must not panic");
    assert_eq!(
        capture.err(),
        Some(isoplane_host::hibernate_policy::CaptureRefused::NotParked),
        "even an empty-stack debug await must refuse capture before invoking GC"
    );
    rig.cell.complete_future(future, ResultRecord::Ok(vec![]));
    let result =
        tokio::task::spawn_blocking(move || receiver.recv_timeout(Duration::from_secs(30)))
            .await
            .expect("response wait must not panic")
            .expect("the completed future must wake the guest");
    assert_eq!(
        result.completion,
        Completion::Done,
        "the shared cell must deliver the debug continuation"
    );
    await_terminal_cleanup(&rig.cell, result.instance).await;
    assert!(
        rig.cell.resident_ids().is_empty(),
        "the resumed debug guest must be discarded"
    );
    assert!(
        rig.cell.futures().live_guest_future_ids().is_empty(),
        "the timer and cancellation ownership must end"
    );
}

async fn await_terminal_cleanup(cell: &Cell, instance: isoplane_host::InstanceId) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while cell.resident_ids().contains(&instance) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the execution owner never released the completed debug instance");
}

#[test]
fn debug_registration_refuses_a_production_wall_backstop() {
    let raw = wat::parse_str(
        r#"(module
        (memory (export "memory") 1 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 1024)
        (func (export "isoplane_initialize") (result i64) i64.const 0)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("raw registration fixture must assemble");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 1,
            max_memory_bytes: 65536,
            guard_size_bytes: 65536,
            ..Default::default()
        })
        .expect("the registration engine must start"),
    );
    let module = ModuleId::new("debug-registration", "v1");
    let activator = Arc::new(
        DebugActivator::load(engine, module.clone(), &raw).expect("the debug fixture must load"),
    );
    let (cell, _) = common::rig_resolving(
        CellConfig::for_test("ordinary"),
        "93.184.216.34:80",
    );
    let rig = Rig { cell, module };
    assert!(
        rig.cell
            .register_debug_module(
                "owner",
                activator,
                common::manifest(
                    "debug-registration",
                    HandlerLifecycle::Fresh,
                    Limits::default()
                )
            )
            .is_err(),
        "debug registration must not silently inherit production wall limits"
    );
}
