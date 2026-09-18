//! Hand-written `.wat` fixtures and a test `HostBindings`. The engine's tests
//! do not use a guest SDK, so a guest-SDK bug is never mistakable for an
//! engine bug and each fixture expresses one behaviour.

use isoplane_engine::{EngineConfig, Instance, IsoplaneEngine, Template};

mod bindings;
pub use bindings::{Recorder, TestBindings};

/// A small engine: the guard is not the default because every test process
/// would otherwise reserve tens of GiB of address space per engine it builds.
pub fn small_engine_config() -> EngineConfig {
    EngineConfig {
        max_instances: 64,
        max_memory_bytes: 4 << 20,
        epoch_tick_us: 200,
        pooling: true,
        spectre_mitigations: true,
        guard_size_bytes: 2 << 20,
        ..EngineConfig::default()
    }
}

pub fn test_engine() -> IsoplaneEngine {
    IsoplaneEngine::new(small_engine_config()).expect("engine")
}

pub fn activate(e: &IsoplaneEngine, t: &Template) -> Instance {
    e.activate(t, Box::new(TestBindings::new(Recorder::default())))
        .expect("activation")
}

/// A bump allocator plus the required exports; `handle` and `wake` are the
/// bodies of `isoplane_handle` and `isoplane_wake`. The allocator wraps rather than
/// grows: the engine copies each record in and the guest never retains it.
pub fn guest(
    pages: u32,
    extra_imports: &str,
    extra_funcs: &str,
    handle: &str,
    wake: &str,
) -> Vec<u8> {
    let heap_top = pages as usize * 65536 - 4096;
    let src = format!(
        r#"
(module
  {extra_imports}
  (memory (export "memory") {pages} {pages})
  (global $bump (export "g_bump") (mut i32) (i32.const 4096))
  (global $calls (export "g_calls") (mut i64) (i64.const 0))

  ;; (size, align) -> ptr. 0 means "cannot allocate".
  (func (export "isoplane_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const {heap_top}))
      (then (global.set $bump (i32.const 4096))))
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (local.get $p) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  (func (export "isoplane_handle") (param $ptr i32) (param $len i32) (result i64)
    (global.set $calls (i64.add (global.get $calls) (i64.const 1)))
    {handle})

  (func (export "isoplane_wake") (param $future i64) (param $ptr i32) (param $len i32) (result i64)
    (global.set $calls (i64.add (global.get $calls) (i64.const 1)))
    {wake})

  ;; Burn `n` iterations of pure arithmetic: CPU with no memory traffic.
  (func $burn (param $n i32) (result i64)
    (local $i i32) (local $x i64)
    (local.set $x (i64.const 88172645463325252))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $n)))
      (local.set $x (i64.xor (local.get $x) (i64.shl (local.get $x) (i64.const 13))))
      (local.set $x (i64.xor (local.get $x) (i64.shr_u (local.get $x) (i64.const 7))))
      (local.set $x (i64.xor (local.get $x) (i64.shl (local.get $x) (i64.const 17))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l)))
    (local.get $x))

  {extra_funcs}
)
"#
    );
    wat::parse_str(&src).expect("fixture .wat must assemble")
}

/// `PollStatus::Done { response: 0 }` as the packed `i64` the ABI expects.
pub const DONE: &str = "(i64.const 0)";
