//! Bounded guest memory cannot be reused to grow unmetered host buffers.

mod common;

use std::mem::size_of;
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{GuestMemoryBytes, Limits};
use isoplane_abi::dispatch::{HttpRequest, ResponseHead};
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::bindings::GuestResourceLimits;
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Target};
use isoplane_host::{EngineActivator, ModuleId};

fn rig(program: &str, resources: GuestResourceLimits) -> (Arc<Cell>, ModuleId) {
    let wasm = wat::parse_str(program).expect("hostile module assembles");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            pooling: false,
            max_memory_bytes: 2 * 65536,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("host-buffers", "v1");
    let activator = Arc::new(
        EngineActivator::load(engine, module.clone(), &wasm)
            .expect("hostile guest passes the real validator"),
    );
    let (cell, _) = common::rig_resolving(
        CellConfig {
            guest_resources: resources,
            ..CellConfig::for_test("host-buffers")
        },
        "93.184.216.34:80",
    );
    let mut manifest = common::manifest("host-buffers", Limits::default());
    manifest.guest_memory_bytes = Some(GuestMemoryBytes::new(65536).expect("one-page allowance"));
    cell.register_module("owner", activator, manifest)
        .expect("register hostile guest");
    (cell, module)
}

fn dispatch(cell: &Cell, module: &ModuleId) -> DispatchResult {
    let record = cell.http_record(
        module,
        HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    );
    cell.dispatch_blocking(
        module,
        Target::Handler { route: "/".into() },
        record,
        Duration::from_secs(10),
    )
    .expect("hostile dispatch completes without hanging")
}

#[test]
fn repeated_trace_names_and_empty_spans_are_bounded_and_released_between_warm_requests() {
    for (name_len, resources) in [
        (
            3,
            GuestResourceLimits {
                trace_spans: 8,
                trace_name_bytes: 6,
                ..Default::default()
            },
        ),
        (
            0,
            GuestResourceLimits {
                trace_spans: 2,
                trace_name_bytes: 1024,
                ..Default::default()
            },
        ),
    ] {
        let (cell, module) = rig(
            &format!(
                r#"(module
            (import "isoplane" "trace_span_begin" (func $begin (param i32 i32) (result i32)))
            (memory (export "memory") 1 1)
            (data (i32.const 1024) "abc")
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
            (func (export "isoplane_handle") (param i32 i32) (result i64)
                (local $n i32)
                (loop $again
                    (if (i32.eqz (call $begin (i32.const 1024) (i32.const {name_len}))) (then unreachable))
                    (local.set $n (i32.add (local.get $n) (i32.const 1)))
                    (br_if $again (i32.lt_u (local.get $n) (i32.const 2))))
                (if (call $begin (i32.const 1024) (i32.const {name_len})) (then unreachable))
                (i64.const 0))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#
            ),
            resources,
        );
        let mut instance = None;
        for _ in 0..4 {
            let result = dispatch(&cell, &module);
            assert_eq!(
                result.completion,
                Completion::Done,
                "the third span is refused, and each warm request gets fresh capacity"
            );
            if let Some(previous) = instance {
                assert_eq!(
                    result.instance, previous,
                    "cleanup occurs on the same warm instance"
                );
            }
            instance = Some(result.instance);
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while cell.idle_instances(&module) != Some(1) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the completed instance never returned to its warm pool"
                );
                std::thread::yield_now();
            }
        }
        cell.shutdown();
    }
}

#[test]
fn repeating_a_small_response_write_cannot_exceed_the_effective_application_buffer_ceiling() {
    let head = postcard::to_allocvec(&ResponseHead {
        status: 200,
        headers: vec![],
        body: vec![],
        streaming: false,
    })
    .expect("encode response head");
    let encoded = isoplane_test_support::wat_bytes(&head);
    let (cell, module) = rig(
        &format!(
            r#"(module
        (import "isoplane" "resp_new" (func $new (param i32 i32) (result i32)))
        (import "isoplane" "resp_write" (func $write (param i32 i32 i32) (result i64)))
        (import "isoplane" "resp_finish" (func $finish (param i32)))
        (memory (export "memory") 1 1)
        (data (i32.const 1024) "{encoded}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (local $response i32) (local $n i32)
            (local.set $response (call $new (i32.const 1024) (i32.const {head_len})))
            (loop $again
                (drop (call $write (local.get $response) (i32.const 8192) (i32.const 4096)))
                (local.set $n (i32.add (local.get $n) (i32.const 1)))
                (br_if $again (i32.lt_u (local.get $n) (i32.const 24))))
            (call $finish (local.get $response))
            (i64.shl (i64.extend_i32_u (local.get $response)) (i64.const 32)))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
            head_len = head.len()
        ),
        GuestResourceLimits::default(),
    );
    let result = dispatch(&cell, &module);
    assert_eq!(
        result.completion,
        Completion::Done,
        "a buffered write refusal does not trap the instance"
    );
    let accepted_bytes = ((65536 - size_of::<isoplane_host::response::Response>()) / 4096) * 4096;
    assert_eq!(result.body.len(), accepted_bytes, "the same 4KiB guest buffer cannot create 96KiB of retained host body under a 64KiB allowance");
    cell.shutdown();
}

#[test]
fn repeated_response_heads_exhaust_the_configured_handle_count() {
    let head = postcard::to_allocvec(&ResponseHead {
        status: 200,
        headers: vec![],
        body: vec![],
        streaming: false,
    })
    .expect("encode response head");
    let encoded = isoplane_test_support::wat_bytes(&head);
    let (cell, module) = rig(
        &format!(
            r#"(module
        (import "isoplane" "resp_new" (func $new (param i32 i32) (result i32)))
        (memory (export "memory") 1 1)
        (data (i32.const 1024) "{encoded}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (drop (call $new (i32.const 1024) (i32.const {head_len})))
            (drop (call $new (i32.const 1024) (i32.const {head_len})))
            (drop (call $new (i32.const 1024) (i32.const {head_len})))
            (i64.const 0))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
            head_len = head.len()
        ),
        GuestResourceLimits {
            response_handles: 2,
            ..Default::default()
        },
    );
    let result = dispatch(&cell, &module);
    assert_eq!(
        result.completion,
        Completion::Trapped,
        "synchronous response allocation failure is contained as a guest trap"
    );
    cell.shutdown();
}
