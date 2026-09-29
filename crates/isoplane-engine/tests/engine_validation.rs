//! Import-surface validation and engine construction refusals.

mod common;

use common::*;
use isoplane_engine::{EngineConfig, IsoplaneEngine};

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "isoplane" "{name}" (func ${name} {params} {results}))"#)
}

// ---------------------------------------------------------------------------
// imports are the complete capability surface
// ---------------------------------------------------------------------------

#[test]
fn an_import_outside_the_table_fails_to_load() {
    let e = test_engine();

    let wasm = guest(
        4,
        r#"(import "isoplane" "definitely_not_an_import" (func $x (result i64)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("an undeclared import must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("definitely_not_an_import"),
        "the error must name the offending import: {msg}"
    );

    let wasm = guest(
        4,
        r#"(import "wasi_snapshot_preview1" "fd_write" (func $x (param i32) (result i32)))"#,
        "",
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("an import from an unknown module must be rejected");
    assert!(
        format!("{err:#}").contains("fd_write"),
        "the error must name the offending import: {err:#}"
    );

    let wasm = guest(
        4,
        &import("service_call", "(param i32 i32)", "(result i64)"),
        "",
        DONE,
        DONE,
    );
    e.load_template(&wasm).expect("a declared import must link");
}

#[test]
fn a_module_missing_a_required_export_cannot_activate() {
    let e = test_engine();
    let src = r#"
(module
  (memory (export "memory") 1)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 1024))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0)))"#;
    let t = e
        .load_template(&wat::parse_str(src).expect("wat"))
        .expect("the module compiles");
    let err = e
        .activate(&t, Box::new(TestBindings::new(Recorder::default())))
        .expect_err("a module without isoplane_wake is not deployable");
    assert!(
        format!("{err:#}").contains("isoplane_wake"),
        "the error must name the missing export: {err:#}"
    );
}

#[test]
fn a_template_whose_start_function_calls_an_import_is_refused() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("init_unit_begin", "(param i32)", ""),
        r#"
  (func $start (call $init_unit_begin (i32.const 1)))
  (start $start)"#,
        DONE,
        DONE,
    );
    let err = e
        .load_template(&wasm)
        .expect_err("a template that reaches the host during instantiation must be refused");
    assert!(
        format!("{err:#}").contains("called a host import"),
        "the error must say the start function reached the host: {err:#}"
    );
}

#[test]
fn a_module_with_more_table_elements_than_the_pool_allows_is_refused() {
    let e = test_engine();
    let wasm = guest(4, "", "(table 131073 funcref)", DONE, DONE);
    e.load_template(&wasm)
        .expect_err("a table over the pool's element cap must not load");
    let wasm = guest(4, "", "(table 131072 funcref)", DONE, DONE);
    let t = e.load_template(&wasm).expect("a table at the cap loads");
    activate(&e, &t);
}

// ---------------------------------------------------------------------------
// engine construction
// ---------------------------------------------------------------------------

#[test]
fn an_epoch_tick_too_coarse_for_the_grace_floor_is_refused_at_construction() {
    let err = IsoplaneEngine::new(EngineConfig {
        epoch_tick_us: 5_000,
        ..small_engine_config()
    })
    .err()
    .expect("a 5 ms tick cannot deliver a 2 ms grace window and must be refused");
    assert!(
        format!("{err:#}").contains("grace"),
        "the refusal must say what the tick is too coarse for: {err:#}"
    );
}

// ---------------------------------------------------------------------------
// the features that make blocking inexpressible
// ---------------------------------------------------------------------------

#[test]
fn threads_shared_memory_and_atomics_are_rejected_by_the_engine() {
    let e = test_engine();

    let cases: [(&str, &str); 4] = [
        (
            "shared memory",
            r#"(module (memory (export "memory") 1 1 shared)
                       (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
                       (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "atomic wait",
            r#"(module (memory (export "memory") 1 1 shared)
                       (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "isoplane_handle") (param i32 i32) (result i64)
                         (drop (memory.atomic.wait32 (i32.const 0) (i32.const 0) (i64.const -1)))
                         (i64.const 0))
                       (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "atomic rmw on unshared memory",
            r#"(module (memory (export "memory") 1)
                       (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "isoplane_handle") (param i32 i32) (result i64)
                         (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
                         (i64.const 0))
                       (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
        (
            "two linear memories",
            r#"(module (memory (export "memory") 1) (memory 1)
                       (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
                       (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
                       (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        ),
    ];

    for (what, src) in cases {
        let wasm = wat::parse_str(src)
            .unwrap_or_else(|err| panic!("the `{what}` fixture must assemble: {err}"));
        assert!(
            e.load_template(&wasm).is_err(),
            "a module using {what} must not load"
        );
    }

    let relaxed = r#"(module (memory (export "memory") 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
          (drop (i32x4.relaxed_trunc_f32x4_s (v128.const i32x4 0 0 0 0)))
          (i64.const 0))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    let wasm = wat::parse_str(relaxed).expect("the relaxed SIMD fixture must assemble");
    assert!(
        e.load_template(&wasm).is_err(),
        "relaxed SIMD must be rejected; only the deterministic subset is allowed"
    );
    let deterministic = r#"(module (memory (export "memory") 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
          (drop (i32x4.add (v128.const i32x4 1 2 3 4) (v128.const i32x4 5 6 7 8)))
          (i64.const 0))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#;
    e.load_template(&wat::parse_str(deterministic).expect("wat"))
        .expect("the deterministic SIMD subset must still load");
}
