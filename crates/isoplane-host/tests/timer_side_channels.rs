//! Hostile modules that drive host timers through the raw `poll_call` import,
//! bypassing any runtime, through the real engine and cell.
//!
//! SECURITY: these guard side-channel mitigations. A timer whose readiness
//! changed while the guest ran would be a clock finer than the frozen slice
//! clock, which is what Spectre-class attacks need; and an instance's timers
//! are capped so a guest cannot multiply the host work it causes.

mod common;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::cell::{CellConfig, DispatchResult, Target};
use isoplane_host::{EngineActivator, ModuleId};
use std::sync::Arc;
use std::time::Duration;

/// The canonical `poll_call` operations these modules use.
const SUBSCRIBE_DURATION: i32 = 0;
const POLLABLE_READY: i32 = 58;

/// Runs one request on a module whose `isoplane_handle` and `isoplane_wake`
/// bodies are given. `$call` makes a one-argument `poll_call`, and the handle
/// of the module's first timer is kept at address 2048.
fn dispatch(handle: &str, wake: &str) -> DispatchResult {
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "isoplane" "poll_call" (func $poll (param i32 i32 i32) (result i64)))
        (memory (export "memory") 2 2)
        (func $call (param $operation i32) (param $argument i64) (result i64)
            (i64.store (i32.const 1024) (local.get $argument))
            (call $poll (local.get $operation) (i32.const 1024) (i32.const 1)))
        (func $ready (result i64)
            (call $call (i32.const {POLLABLE_READY}) (i64.load32_u (i32.const 2048))))
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64) (local $n i32) {handle})
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) {wake})
    )"#
    ))
    .expect("the hostile module must assemble");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("timer-side-channels", "v1");
    let activator = Arc::new(
        EngineActivator::load(engine, module.clone(), &wasm)
            .expect("a module importing poll_call must link"),
    );
    let (cell, _) = common::rig_resolving(
        CellConfig::for_test("timer-side-channels"),
        "93.184.216.34:80",
    );
    cell.register_module(
        "owner",
        activator,
        common::manifest("timer-side-channels", Limits::default()),
    )
    .expect("register the hostile module");
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
            Duration::from_secs(30),
        )
        .expect("the dispatch must finish before its hang deadline");
    cell.shutdown();
    result
}

#[test]
fn a_timer_polled_in_a_loop_never_fires_within_the_slice_that_armed_it() {
    // Arms a 1 ns timer and polls it 200,000 times, far longer than the timer
    // runs; seeing it ready traps. Then it parks, and the wake must find it
    // ready: the timer fires, only never mid-slice.
    let result = dispatch(
        &format!(
            r#"(i32.store (i32.const 2048)
                (i32.wrap_i64 (call $call (i32.const {SUBSCRIBE_DURATION}) (i64.const 1))))
            (loop $poll
                (if (i64.ne (call $ready) (i64.const 0)) (then unreachable))
                (local.set $n (i32.add (local.get $n) (i32.const 1)))
                (br_if $poll (i32.lt_u (local.get $n) (i32.const 200000))))
            (i64.const 1)"#
        ),
        "(if (i64.eqz (call $ready)) (then unreachable)) (i64.const 0)",
    );
    assert_eq!(
        (result.completion, result.status, result.slices),
        (Completion::Done, 204, 2),
        "the timer must stay unready for the whole slice and be ready in the next: {result:?}"
    );
}

/// A module that arms `count` one-second timers and returns.
fn arming(count: u32) -> DispatchResult {
    dispatch(
        &format!(
            r#"(loop $arm
                (drop (call $call (i32.const {SUBSCRIBE_DURATION}) (i64.const 1000000000)))
                (local.set $n (i32.add (local.get $n) (i32.const 1)))
                (br_if $arm (i32.lt_u (local.get $n) (i32.const {count}))))
            (i64.const 0)"#
        ),
        "unreachable",
    )
}

#[test]
fn an_instance_holds_at_most_sixteen_timers() {
    let limit = isoplane_abi::native_io::MAX_TIMERS as u32;
    let within = arming(limit);
    assert_eq!(
        (within.completion, within.status),
        (Completion::Done, 204),
        "{limit} timers must be accepted: {within:?}"
    );
    let over = arming(limit + 1);
    assert_eq!(
        over.completion,
        Completion::Trapped,
        "timer {} must be refused: {over:?}",
        limit + 1
    );
}

#[test]
fn no_import_exposes_the_remaining_budget() {
    // A budget that falls while the guest runs is a clock; a guest learns its
    // budget ran out through cancellation at its next slice instead.
    let wasm = wat::parse_str(
        r#"(module
        (import "isoplane" "budget_remaining_ns" (func $remaining (result i64)))
        (memory (export "memory") 1 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "isoplane_handle") (param i32 i32) (result i64) (call $remaining))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
    )"#,
    )
    .expect("the hostile module must assemble");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    assert!(
        EngineActivator::load(engine, ModuleId::new("budget-reader", "v1"), &wasm).is_err(),
        "a module importing budget_remaining_ns must be refused"
    );
}
