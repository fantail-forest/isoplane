//! The memory query and enforcement share the effective instance ceiling.
#[path = "common/bindings.rs"]
mod bindings;

use bindings::{Recorder, TestBindings};
use isoplane_abi::WASM_PAGE_SIZE;
use isoplane_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, IsoplaneEngine};

fn observed_limit(
    module_pages: Option<u64>,
    engine_pages: u64,
    application_pages: Option<u64>,
    expected_pages: u64,
) {
    let maximum = module_pages
        .map(|pages| pages.to_string())
        .unwrap_or_default();
    let growth_pages = expected_pages - 1;
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "isoplane" "memory_limit_bytes" (func $limit (result i64)))
        (memory (export "memory") 1 {maximum})
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_initialize") (result i64)
            (i64.store (i32.const 0) (call $limit))
            (if (i32.ne (memory.grow (i32.const {growth_pages})) (i32.const 1))
                (then unreachable))
            (if (i64.ne (call $limit) (i64.load (i32.const 0)))
                (then unreachable))
            (if (i32.ne (memory.grow (i32.const 1)) (i32.const -1))
                (then unreachable))
            i64.const 0))"#
    ))
    .expect("memory query fixture must assemble");
    for pooling in [false, true] {
        let engine = IsoplaneEngine::new(EngineConfig {
            max_instances: 1,
            max_memory_bytes: engine_pages * WASM_PAGE_SIZE as u64,
            pooling,
            ..EngineConfig::default()
        })
        .expect("the memory query engine must start");
        let template = engine
            .load_template(&wasm)
            .expect("the memory query fixture must load");
        let mut bindings = TestBindings::new(Recorder::default());
        bindings.init_sandbox = true;
        bindings.memory_limit_bytes = application_pages.map(|pages| pages * WASM_PAGE_SIZE as u64);
        let mut instance = engine
            .activate(&template, Box::new(bindings))
            .expect("the memory query fixture must activate");
        let outcome = instance.run_initialize(CpuBudget::http());
        assert_eq!(outcome.completion, Completion::Done,
        "initialization must query its limit and survive growth refused by that same ceiling: {outcome:?}");
        let reported =
            u64::from_le_bytes(instance.memory()[..8].try_into().expect("eight-byte limit"));
        assert_eq!(reported, expected_pages * WASM_PAGE_SIZE as u64,
        "the runtime must see the total enforceable memory ceiling before application initialization");
    }
}

#[test]
fn the_memory_query_observes_the_application_ceiling() {
    observed_limit(Some(8), 16, Some(4), 4);
}

#[test]
fn the_memory_query_observes_a_lower_engine_capacity() {
    observed_limit(Some(8), 4, Some(6), 4);
}

#[test]
fn the_memory_query_observes_a_lower_module_maximum() {
    observed_limit(Some(4), 16, Some(8), 4);
}

#[test]
fn an_unbounded_module_reports_the_engine_capacity() {
    observed_limit(None, 8, None, 8);
}
