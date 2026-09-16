//! Captured initialization state restores over unchanged compiled code.
#![cfg(target_os = "linux")]
mod common;

use common::*;
use wisp_engine::prepared::{PreparedIdentity, PreparedStorage, PreparedTemplate};
use wisp_engine::{Completion, CpuBudget, Instance, InstanceOps, Template, WispEngine};

fn identity() -> PreparedIdentity {
    PreparedIdentity {
        input_hash: [1; 32],
        runtime_profile: [2; 32],
        initialization_hash: [3; 32],
    }
}

fn source(engine: &WispEngine, body: &str) -> (Vec<u8>, Template) {
    let wasm = guest(
        1,
        "",
        &format!(
            r#"
          (data (i32.const 100) "original")
          (func (export "wisp_initialize") (result i64) {body})
        "#
        ),
        DONE,
        DONE,
    );
    let template = engine.load_layer_source(&wasm).expect("mapped source");
    (wasm, template)
}

fn capture(engine: &WispEngine, source: &Template, instance: &mut Instance) -> PreparedTemplate {
    engine
        .prepare_initialized_template(source, instance, identity(), Default::default(), vec![])
        .expect("capture completed initializer")
}

fn initialize(engine: &WispEngine, source: &Template) -> Instance {
    let mut instance = activate(engine, source);
    assert_eq!(
        instance.run_initialize(CpuBudget::http()).completion,
        Completion::Done,
        "initializer must complete before capture"
    );
    instance
}

#[test]
fn layer_publication_preserves_compiled_code_and_restores_globals_and_memory() {
    let engine = test_engine();
    let (wasm, source) = source(
        &engine,
        "i32.const 100 i32.const 42 i32.store8 i64.const 73 global.set $calls i64.const 0",
    );
    let (_, native_before) = engine.precompile(&wasm).expect("original native code");
    let mut instance = initialize(&engine, &source);
    let captured = instance.capture_mutable_state().expect("captured globals");
    let product = capture(&engine, &source, &mut instance);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("layer");
    product
        .publish(&path, PreparedStorage::Mapped)
        .expect("publish");
    assert_eq!(
        std::fs::read(path.join("template.cwasm")).expect("native payload"),
        native_before,
        "layer capture must preserve the original compiled native bytes"
    );
    drop(instance);
    drop(source);
    // SAFETY: the test owns the published files and never changes their contents.
    let restored = unsafe { engine.load_prepared_mapped(&path, identity(), &[]) }
        .expect("load layer from published files");
    assert!(
        !restored.has_initialize(),
        "activation must not rerun captured initialization"
    );
    let mut fresh = activate(&engine, &restored);
    assert_eq!(
        fresh.memory()[100],
        42,
        "activation must restore initializer writes"
    );
    assert_eq!(
        postcard::to_allocvec(&fresh.capture_mutable_state().expect("restored globals"))
            .expect("encode"),
        postcard::to_allocvec(&captured).expect("encode"),
        "activation must restore captured globals"
    );
    assert_eq!(
        restored.memory_image_arc().to_dense(),
        fresh.memory(),
        "hibernation must compare against post-initialization memory"
    );
}

#[test]
fn layer_private_writes_do_not_survive_pool_slot_reuse() {
    let engine = WispEngine::new(wisp_engine::EngineConfig {
        max_instances: 1,
        ..small_engine_config()
    })
    .expect("one-slot engine");
    let (_, source) = source(&engine, "i32.const 100 i32.const 42 i32.store8 i64.const 0");
    let mut instance = initialize(&engine, &source);
    let product = capture(&engine, &source, &mut instance);
    drop(instance);
    let restored = engine.load_prepared_product(&product).expect("load layer");
    for _ in 0..3 {
        let mut fresh = activate(&engine, &restored);
        assert_eq!(
            fresh.memory()[100],
            42,
            "private writes must not contaminate the next activation"
        );
        fresh.memory()[100] = 88;
    }
    let mut original = activate(&engine, &source);
    assert_eq!(
        original.memory()[100],
        b'o',
        "reusing the layer slot for original code must restore its original baseline"
    );
}

#[test]
fn layer_capture_refuses_unfinished_or_unrelated_instances() {
    let engine = test_engine();
    let (_, source) = source(&engine, "i64.const 0");
    let mut instance = activate(&engine, &source);
    assert!(
        engine
            .prepare_initialized_template(
                &source,
                &mut instance,
                identity(),
                Default::default(),
                vec![]
            )
            .is_err(),
        "an instance that never completed initialization must not be captured"
    );
    assert_eq!(
        instance.run_initialize(CpuBudget::http()).completion,
        Completion::Done,
        "initializer must finish"
    );
    let other = engine
        .load_layer_source(&guest(1, "", "", DONE, DONE))
        .expect("other source");
    assert!(
        engine
            .prepare_initialized_template(
                &other,
                &mut instance,
                identity(),
                Default::default(),
                vec![]
            )
            .is_err(),
        "captured state must belong to the supplied compiled source"
    );
}

#[test]
fn activation_restores_memory_grown_by_initialization() {
    let engine = test_engine();
    let wasm = wat::parse_str(
        r#"(module
      (memory (export "memory") 1 4)
      (data (i32.const 100) "original")
      (global (export "counter") (mut i64) (i64.const 0))
      (func (export "wisp_alloc") (param i32 i32) (result i32) i32.const 4096)
      (func (export "wisp_handle") (param i32 i32) (result i64) i64.const 0)
      (func (export "wisp_wake") (param i64 i32 i32) (result i64) i64.const 0)
      (func (export "wisp_initialize") (result i64)
        i32.const 1 memory.grow drop
        i32.const 100000 i32.const 61 i32.store8
        i64.const 0))"#,
    )
    .expect("growing fixture");
    let source = engine.load_layer_source(&wasm).expect("source");
    let mut instance = initialize(&engine, &source);
    let product = capture(&engine, &source, &mut instance);
    let restored = engine.load_prepared_product(&product).expect("load layer");
    let mut fresh = activate(&engine, &restored);
    assert_eq!(
        fresh.memory().len(),
        2 * 65536,
        "activation must restore captured memory growth"
    );
    assert_eq!(
        fresh.memory()[100000],
        61,
        "grown pages must come from the captured layer"
    );
    assert!(
        restored
            .memory_image_arc()
            .page_eq(100000 / 4096, &fresh.memory()[98304..102400]),
        "hibernation comparisons must recognize unchanged grown pages"
    );
}

#[test]
fn capture_refuses_an_initializer_that_changed_a_function_table() {
    let engine = test_engine();
    let wasm = guest(
        1,
        "",
        r#"
      (table (export "table") 1 funcref)
      (func $marker)
      (elem (i32.const 0) $marker)
      (func (export "wisp_initialize") (result i64)
        i32.const 0 ref.null func table.set 0 i64.const 0)
    "#,
        DONE,
        DONE,
    );
    let source = engine.load_layer_source(&wasm).expect("source");
    let mut instance = initialize(&engine, &source);
    assert!(
        engine
            .prepare_initialized_template(
                &source,
                &mut instance,
                identity(),
                Default::default(),
                vec![]
            )
            .is_err(),
        "function-table changes cannot be silently lost when capturing a layer"
    );
}

#[test]
fn simultaneous_layer_instances_keep_private_writes_isolated() {
    let engine = test_engine();
    let (_, source) = source(&engine, "i32.const 100 i32.const 42 i32.store8 i64.const 0");
    let mut initialized = initialize(&engine, &source);
    let product = capture(&engine, &source, &mut initialized);
    drop(initialized);
    let template = engine.load_prepared_product(&product).expect("layer");
    let mut first = activate(&engine, &template);
    let mut second = activate(&engine, &template);
    first.memory()[100] = 77;
    assert_eq!(
        second.memory()[100],
        42,
        "another live instance must retain its own snapshot bytes"
    );
    second.memory()[100] = 88;
    assert_eq!(
        first.memory()[100],
        77,
        "private writes must remain isolated in both directions"
    );
}
