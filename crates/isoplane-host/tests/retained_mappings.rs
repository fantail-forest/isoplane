//! Registered template mappings survive hibernation while mutable state comes from the delta.
#![cfg(target_os = "linux")]
mod common;

use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{
    Completion, EngineConfig, HostBindings, ImportCall, IsoplaneEngine, OccupancyBudget,
};
use isoplane_host::cell::{CellConfig, Target};
use isoplane_host::{EngineActivator, LifecycleTier, ModuleId};
use std::sync::Arc;
use std::time::Duration;

struct NoImports;
impl HostBindings for NoImports {
    fn begin_slice(&mut self) {}
    fn end_slice(&mut self) {}
    fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
        panic!("fixture has no imports")
    }
    fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
        panic!("fixture has no imports")
    }
    fn slice_clock_ms(&self) -> u64 {
        0
    }
}

#[test]
fn a_hibernated_guest_restores_its_delta_into_the_same_mapping() {
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 2,
            max_memory_bytes: 4 << 20,
            guard_size_bytes: 2 << 20,
            ..EngineConfig::default()
        })
        .expect("engine"),
    );
    let wasm = wat::parse_str(
        r#"(module
      (memory (export "memory") 1 4)
      (global $count (export "count") (mut i32) (i32.const 40))
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
      (func (export "isoplane_initialize") (result i64)
        (i32.store (i32.const 64) (global.get $count)) (i64.const 0))
      (func (export "isoplane_handle") (param i32 i32) (result i64)
        (if (i32.ne (i32.load (i32.const 64)) (global.get $count)) (then unreachable))
        (drop (memory.grow (i32.const 1)))
        (global.set $count (i32.add (global.get $count) (i32.const 1)))
        (i32.store (i32.const 64) (global.get $count)) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("fixture");
    let source = engine.load_layer_source(&wasm).expect("source");
    let mut initialization = engine
        .activate(&source, Box::new(NoImports))
        .expect("initialize");
    assert_eq!(
        initialization
            .run_initialize(OccupancyBudget::default())
            .completion,
        Completion::Done,
        "initialization must finish"
    );
    let product = engine
        .prepare_initialized_template(
            &source,
            &mut initialization,
            PreparedIdentity {
                input_hash: [1; 32],
                runtime_profile: [2; 32],
                initialization_hash: [3; 32],
            },
            Default::default(),
            vec![],
        )
        .expect("capture");
    drop(initialization);
    let template = engine.load_prepared_product(&product).expect("template");
    let module = ModuleId::new("retained", "v1");
    let (cell, _) = common::rig_resolving(CellConfig::for_test("retained"), "93.184.216.34:80");
    cell.register_module(
        "owner",
        Arc::new(EngineActivator::new(engine, template, module.clone())),
        common::manifest("retained"),
    )
    .expect("registration");
    let mut address = None;
    for expected in 41..=43u32 {
        let request = HttpRequest {
            method: "GET".into(),
            url: "https://retained.test/".into(),
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
                Duration::from_secs(30),
            )
            .expect("dispatch");
        assert_eq!(result.status, 204, "restored globals and memory must agree");
        let id = cell.resident_ids()[0];
        let slot = cell.slot(id).expect("slot");
        let mut guard = slot.enter().expect("guest must be idle after dispatch");
        let memory = guard.instance().memory();
        assert_eq!(
            u32::from_le_bytes(memory[64..68].try_into().expect("counter bytes")),
            expected,
            "hibernation must preserve the previous dispatch's changes"
        );
        assert_eq!(
            memory.len(),
            ((expected - 39) as usize) * 65536,
            "hibernation must preserve memory growth beyond the template"
        );
        let current = memory.as_ptr() as usize;
        assert_eq!(
            *address.get_or_insert(current),
            current,
            "wake must retain the original linear-memory address"
        );
        drop(guard);
        cell.force_hibernate(id)
            .expect("checkpoint before releasing pages");
        assert_eq!(
            cell.instance_state(id),
            Some(LifecycleTier::Hibernated),
            "guest must hibernate"
        );
        assert!(
            slot.enter().is_err(),
            "a dormant mapping must not permit guest entry before delta restoration"
        );
    }
    cell.shutdown();
}
