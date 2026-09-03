//! Hand-written `.wat` fixtures and a test `HostBindings`. The engine's tests
//! do not use a guest SDK, so a guest-SDK bug is never mistakable for an
//! engine bug and each fixture expresses one behaviour.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wisp_engine::{EngineConfig, HostBindings, ImportCall, Instance, Template, WispEngine};

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
    }
}

pub fn test_engine() -> WispEngine {
    WispEngine::new(small_engine_config()).expect("engine")
}

pub fn activate(e: &WispEngine, t: &Template) -> Instance {
    e.activate(t, Box::new(TestBindings::new(Recorder::default())))
        .expect("activation")
}

/// A bump allocator plus the required exports; `handle` and `wake` are the
/// bodies of `wisp_handle` and `wisp_wake`. The allocator wraps rather than
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
  (func (export "wisp_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const {heap_top}))
      (then (global.set $bump (i32.const 4096))))
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (local.get $p) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  (func (export "wisp_handle") (param $ptr i32) (param $len i32) (result i64)
    (global.set $calls (i64.add (global.get $calls) (i64.const 1)))
    {handle})

  (func (export "wisp_wake") (param $future i64) (param $ptr i32) (param $len i32) (result i64)
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

/// Records what the guest asked for, and hands back a coarse clock that
/// advances on every read, so a frozen-clock test is testing the engine's
/// freeze rather than a host that returned the same number twice.
#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Arc<parking_lot::Mutex<Vec<String>>>,
    pub clock: Arc<AtomicU64>,
    pub clock_reads: Arc<AtomicU64>,
}

pub struct TestBindings {
    pub rec: Recorder,
    pub init_sandbox: bool,
    pub next_future: u64,
    pub fill_byte: u8,
    /// When false, `call_fill` denies, which is the default `HostBindings`
    /// behaviour.
    pub allow_fill: bool,
    /// What `call_fill` reports as the value's full length, which for
    /// `init_config_get` may exceed the capacity the guest offered.
    pub full_length: Option<i64>,
}

impl TestBindings {
    pub fn new(rec: Recorder) -> Self {
        Self {
            rec,
            init_sandbox: false,
            next_future: 1,
            fill_byte: 0xAB,
            allow_fill: true,
            full_length: None,
        }
    }
    fn note(&self, call: &ImportCall<'_>) {
        self.rec
            .calls
            .lock()
            .push(format!("{}({} bytes)", call.import, call.payload.len()));
    }
}

impl HostBindings for TestBindings {
    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        self.note(&call);
        let id = self.next_future;
        self.next_future += 1;
        id
    }

    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        self.note(&call);
        0
    }

    fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> i64 {
        self.note(&call);
        if !self.allow_fill {
            return -1;
        }
        // A fake CSPRNG that answered the same bytes every time could not tell
        // a per-call draw from a cached one.
        out.fill(self.fill_byte);
        self.fill_byte = self.fill_byte.wrapping_add(1);
        self.full_length.unwrap_or(out.len() as i64)
    }

    fn slice_clock_ms(&self) -> u64 {
        self.rec.clock_reads.fetch_add(1, Ordering::Relaxed);
        self.rec.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_init_sandbox(&self) -> bool {
        self.init_sandbox
    }
}
