//! Post-response execution restrictions through the actual WASM engine and cell.
//! Production daemon policy is qualified on its Linux deployment platform.

#![cfg(target_os = "linux")]

mod common;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Target};
use isoplane_host::{EngineActivator, ModuleId};
use std::sync::Arc;
use std::time::Duration;

fn dispatch(handle: &str, wake: &str) -> (Arc<Cell>, DispatchResult) {
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (import "isoplane" "wait_until" (func $retain (param i64)))
        (memory (export "memory") 2 2)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64) {handle})
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) {wake})
    )"#
    ))
    .expect("the fixture must encode the actual guest imports");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("background", "v1");
    let activator = Arc::new(
        EngineActivator::load(engine, module.clone(), &wasm)
            .expect("the unchanged wait_until ABI must still link"),
    );
    let (cell, _) = common::rig_resolving(
        CellConfig::for_test("post-response"),
        "93.184.216.34:80",
    );
    cell.register_module(
        "owner",
        activator,
        common::manifest("background", Limits::default()),
    )
    .expect("register real guest");
    let record = cell.http_record(
        &module,
        HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    );
    let result = cell
        .dispatch_blocking(
            &module,
            Target::Handler { route: "/".into() },
            record,
            Duration::from_secs(10),
        )
        .expect("the real dispatch must finish before its hang deadline");
    (cell, result)
}

async fn await_settlement(cell: &Cell) {
    // The response is sent before cleanup; this metric is recorded after end_dispatch.
    tokio::time::timeout(Duration::from_secs(10), async {
        while cell.metrics_raw().request_us.count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the completed dispatch never settled");
}

#[tokio::test]
async fn a_real_guest_cannot_retain_a_timer_after_its_response() {
    let (cell, result) = dispatch(
        "(call $retain (call $timer (i64.const 600000))) (i64.const 0)",
        "unreachable",
    );
    assert_eq!(
        result.completion,
        Completion::Trapped,
        "retention must fail even when the guest ignores the void import and returns done"
    );
    assert!(
        result
            .body
            .windows(b"CapabilityViolation".len())
            .any(|part| part == b"CapabilityViolation"),
        "the response must explain the capability refusal: {:?}",
        result.body
    );
    await_settlement(&cell).await;
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "no timer may remain available to wake the completed dispatch"
    );
    cell.shutdown();
}

#[tokio::test]
async fn a_real_guest_can_still_await_a_timer_before_completing() {
    let (cell, result) = dispatch(
        "(drop (call $timer (i64.const 0))) (i64.const 1)",
        "(i64.const 0)",
    );
    assert_eq!(
        result.completion,
        Completion::Done,
        "normal awaited timer work remains supported"
    );
    assert_eq!(
        result.slices, 2,
        "the real request must park and resume through a host-owned timer"
    );
    await_settlement(&cell).await;
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "the completed request must release its host futures"
    );
    cell.shutdown();
}
