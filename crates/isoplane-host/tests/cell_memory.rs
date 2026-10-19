//! Application memory budgets enforced independently of the engine's capacity.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{GuestMemoryBytes, Limits};
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};
use isoplane_host::cell::Target;
use isoplane_host::{CellConfig, EngineActivator, ModuleId};

mod common;

const PAGE: u64 = 65536;

/// Grows a one-page guest by `grow_pages`, checks memory.grow's result, and
/// how many instances were retired for passing their memory limit.
fn dispatch_growth(
    memory_override: Option<u64>,
    engine_bytes: u64,
    grow_pages: u64,
    expected_growth: i32,
    expected_retirements: u64,
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
        // macOS network mounts can require writing every reserved byte eagerly.
        #[cfg(target_os = "macos")]
        let snapshots = tempfile::tempdir_in("/tmp").expect("create native snapshot storage");
        #[cfg(not(target_os = "macos"))]
        let snapshots = tempfile::tempdir().expect("create snapshot storage");
        let (services, _) = common::services_resolving("93.184.216.34:80");
        let cell = isoplane_host::cell::Cell::with_snapshot_storage(
            CellConfig {
                ..CellConfig::for_test("memory-budget")
            },
            services,
            Arc::new(
                isoplane_hibernate::snapshot::LocalSnapshotStorage::new(snapshots.path())
                    .expect("open snapshot storage"),
            ),
        )
        .expect("a cell starts");
        let mut manifest = common::manifest(
            "memory-budget",
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 52428800,
                instance_memory_max: 134217728,
                occupancy_ms_per_dispatch: 100,
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
        assert_eq!(
            cell.metrics().memory_limit_retirements,
            expected_retirements,
            "an instance is retired exactly when its memory passes its limit"
        );
        if expected_retirements > 0 {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while cell.slot(result.instance).is_some() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "a retired instance must be discarded once its dispatch ends"
                );
                std::thread::yield_now();
            }
        }
        drop(cell);
        snapshots
            .close()
            .expect("remove the test's snapshot storage");
    }
}

#[test]
fn a_guest_without_a_declared_maximum_cannot_grow_past_its_limits_ceiling() {
    let limit = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    let ceiling = isoplane_abi::config::memory_ceiling_bytes(limit);
    // The engine could hold twice the plan's limit, so only the ceiling refuses.
    dispatch_growth(None, 2 * limit, ceiling / PAGE, -1, 0);
}

#[test]
fn a_guest_may_grow_past_its_limit_up_to_its_ceiling_and_its_instance_is_retired() {
    let limit = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    let ceiling = isoplane_abi::config::memory_ceiling_bytes(limit);
    dispatch_growth(None, 2 * limit, ceiling / PAGE - 1, 1, 1);
}

#[test]
fn an_operator_memory_override_allows_growth_above_the_plan_default() {
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    dispatch_growth(Some(2 * ceiling), 2 * ceiling, ceiling / PAGE, 1, 0);
}

#[test]
fn the_engine_capacity_still_bounds_a_larger_application_override() {
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .instance_memory_max;
    dispatch_growth(Some(2 * ceiling), ceiling, ceiling / PAGE, -1, 0);
}

#[test]
fn a_template_larger_than_the_application_budget_is_refused_at_activation() {
    // One page more than the application's plan allows, within the engine.
    let ceiling = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_dispatch: 100,
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
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 52428800,
                instance_memory_max: 134217728,
                occupancy_ms_per_dispatch: 100,
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

#[test]
fn a_dispatch_does_not_join_an_instance_past_its_memory_limit() {
    let (cell, _) =
        common::rig_resolving(CellConfig::for_test("memory-retiring"), "93.184.216.34:80");
    // Four pages of memory against a one-page limit: past it from the first slice.
    let mut behaviour =
        isoplane_host::fakes::FakeBehaviour::program([isoplane_host::fakes::Step::Park {
            imports: vec![("clock_deadline_in", 60_000)],
        }]);
    behaviour.memory_bytes = 4 * PAGE as usize;
    let activator = Arc::new(isoplane_host::fakes::FakeActivator::new(behaviour));
    let module = isoplane_host::Activator::module(&*activator).clone();
    let mut manifest = common::manifest("memory-retiring", Limits::default());
    manifest.guest_memory_bytes = Some(GuestMemoryBytes::new(PAGE).expect("one page"));
    cell.register_module("tenant", activator, manifest)
        .expect("application registers");
    let submit = || {
        let request = isoplane_abi::HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        cell.submit(
            &module,
            Target::Handler { route: "/".into() },
            cell.http_record(&module, request),
        )
        .expect("the cell admits the dispatch")
    };
    let _first = submit();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while cell.metrics().memory_limit_retirements == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first instance was never retired"
        );
        std::thread::yield_now();
    }
    let _second = submit();
    while cell.metrics().served_activated < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the second dispatch never got an instance"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        cell.metrics().served_shared,
        0,
        "a retiring instance takes no new dispatches"
    );
    cell.shutdown();
}
