//! Application memory budgets enforced independently of the engine's capacity.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{GuestMemoryBytes, HandlerLifecycle, Limits};
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::cell::Target;
use isoplane_host::{CellConfig, EngineActivator, ModuleId};

mod common;

const PAGE: u64 = 65536;

/// Grows a one-page guest by `grow_pages` and checks memory.grow's result.
fn dispatch_growth(
    memory_override: Option<u64>,
    engine_bytes: u64,
    grow_pages: u64,
    expected_growth: i32,
) {
    let wasm = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
            (func (export "isoplane_handle") (param i32 i32) (result i64)
                (if (i32.ne (memory.grow (i32.const {grow_pages})) (i32.const {expected_growth}))
                    (then unreachable))
                i64.const 0)
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#
    ))
    .expect("memory growth fixture assembles");
    for pooling in [false, true] {
        let engine = Arc::new(
            IsoplaneEngine::new(EngineConfig {
                pooling,
                max_instances: 2,
                max_memory_bytes: engine_bytes,
                guard_size_bytes: 65536,
                ..Default::default()
            })
            .expect("engine accepts memory capacity"),
        );
        let module = ModuleId::new("memory-budget", "v1");
        let activator = Arc::new(
            EngineActivator::load(engine, module.clone(), &wasm)
                .expect("small initial memory fits the engine"),
        );
        let (cell, _) = common::rig_resolving(
            CellConfig {
                ..CellConfig::for_test("memory-budget")
            },
            "93.184.216.34:80",
        );
        let mut manifest = common::manifest(
            "memory-budget",
            HandlerLifecycle::Reuse,
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 31457280,
                instance_memory_max: 134217728,
                cpu_ms_per_http_dispatch: 100,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
        );
        manifest.guest_memory_bytes = memory_override
            .map(|bytes| GuestMemoryBytes::new(bytes).expect("override is a valid memory size"));
        cell.register_module("tenant", activator, manifest)
            .expect("application registers");
        let request = isoplane_abi::HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        let result = cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                cell.http_record(&module, request),
                Duration::from_secs(10),
            )
            .expect("memory refusal remains in the guest");
        assert_eq!(
            result.completion,
            Completion::Done,
            "memory.grow must observe the application ceiling under either allocator"
        );
    }
}

#[test]
fn a_guest_without_a_declared_maximum_cannot_grow_past_its_plan() {
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 31457280,
        instance_memory_max: 134217728,
        cpu_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    // The engine could hold twice the plan's ceiling, so only the plan refuses.
    dispatch_growth(None, 2 * ceiling, ceiling / PAGE, -1);
}

#[test]
fn an_operator_memory_override_allows_growth_above_the_plan_default() {
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 31457280,
        instance_memory_max: 134217728,
        cpu_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    dispatch_growth(Some(2 * ceiling), 2 * ceiling, ceiling / PAGE, 1);
}

#[test]
fn the_engine_capacity_still_bounds_a_larger_application_override() {
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 31457280,
        instance_memory_max: 134217728,
        cpu_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    dispatch_growth(Some(2 * ceiling), ceiling, ceiling / PAGE, -1);
}

#[test]
fn a_template_larger_than_the_application_budget_is_refused_at_activation() {
    // One page more than the application's plan allows, within the engine.
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 31457280,
        instance_memory_max: 134217728,
        cpu_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    let wasm = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") {})
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#,
        ceiling / PAGE + 1
    ))
    .expect("large template fixture assembles");
    for pooling in [false, true] {
        let engine = IsoplaneEngine::new(EngineConfig {
            pooling,
            max_instances: 2,
            max_memory_bytes: 2 * ceiling,
            guard_size_bytes: 65536,
            ..Default::default()
        })
        .expect("engine fits template");
        let template = engine
            .load_template(&wasm)
            .expect("template fits the engine capacity");
        let context = Arc::new(isoplane_host::bindings::InstanceCtx::for_test(
            isoplane_host::InstanceId(1),
            HandlerLifecycle::Reuse,
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 31457280,
                instance_memory_max: 134217728,
                cpu_ms_per_http_dispatch: 100,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
        ));
        assert!(
            engine
                .activate(
                    &template,
                    Box::new(isoplane_host::bindings::CellBindings::new(context))
                )
                .is_err(),
            "an oversized template cannot bypass the application's memory budget during activation"
        );
    }
}
