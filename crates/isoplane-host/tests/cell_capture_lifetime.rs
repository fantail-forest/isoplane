//! Capture cannot execute guest callbacks after the dispatch has completed.

mod common;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, Target};
use isoplane_host::hibernate_policy::CaptureRefused;
use isoplane_host::{LifecycleTier, ModuleId};
use std::sync::Arc;
use std::time::Duration;

fn rig(pending: bool) -> (Arc<Cell>, ModuleId) {
    let handle = if pending {
        "(drop (call $timer (i64.const 600000))) (i64.const 1)"
    } else {
        "(i64.const 0)"
    };
    let module = wat::parse_str(format!(
        r#"(module
          (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
          (memory (export "memory") 2 2)
          (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
          (func (export "isoplane_handle") (param i32 i32) (result i64) {handle})
          (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
          (func (export "isoplane_gc_hint") (param i32) unreachable))"#
    ))
    .unwrap();
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 4,
            max_memory_bytes: 2 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .unwrap(),
    );
    let id = ModuleId::new("capture-lifetime", "v1");
    let activator = EngineActivator::load(engine, id.clone(), &module).unwrap();
    let (cell, _) = common::rig_resolving(
        CellConfig::for_test("capture-lifetime"),
        "93.184.216.34:80",
    );
    cell.register_module(
        "owner",
        Arc::new(activator),
        common::manifest("capture-lifetime", Limits::default()),
    )
    .unwrap();
    (cell, id)
}

fn request() -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url: "https://capture.test/".into(),
        path: "/".into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    }
}

#[test]
fn capturing_a_completed_guest_does_not_invoke_its_trapping_gc_hook() {
    let (cell, module) = rig(false);
    let result = cell
        .dispatch_blocking(
            &module,
            Target::Handler { route: "/".into() },
            cell.http_record(&module, request()),
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(result.status, 204, "the guest must complete its request");
    let instance = cell.resident_ids()[0];
    cell.force_hibernate(instance)
        .expect("completed guest capture must not enter its trapping GC callback");
    assert_eq!(
        cell.instance_state(instance),
        Some(LifecycleTier::Hibernated),
        "completed state must remain capturable without executing guest code"
    );
}

#[tokio::test]
async fn capturing_an_unfinished_request_still_invokes_its_gc_hook() {
    let (cell, module) = rig(true);
    let _response = cell
        .submit(
            &module,
            Target::Handler { route: "/".into() },
            cell.http_record(&module, request()),
        )
        .unwrap();
    // This timeout only fails a hung test; no performance threshold is asserted.
    let instance = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if !cell.futures().live_guest_future_ids().is_empty() {
                if let Some(instance) = cell.resident_ids().first().copied() {
                    if cell.instance_state(instance) == Some(LifecycleTier::Parked) {
                        break instance;
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the request never parked on its host timer");
    let outcome = tokio::task::spawn_blocking(move || cell.force_hibernate(instance))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        Err(CaptureRefused::GcHintDidNotComplete(Completion::Trapped)),
        "an unfinished request's capture must still invoke and contain its trapping GC callback"
    );
}
