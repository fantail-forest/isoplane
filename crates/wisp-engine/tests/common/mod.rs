//! Hand-written `.wat` fixtures and a test `HostBindings`.
//!
//! The engine's own tests deliberately do not use the real guest SDK: a
//! guest-SDK bug must never be mistakable for an engine bug, and these fixtures
//! let a test express exactly one behaviour (spin forever, park, read the clock
//! twice) with nothing else in the module. Real guests are used only for the
//! density and latency benches, where module realism is the point.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use wisp_engine::{HostBindings, ImportCall};

/// Bump allocator + the four required exports. `$BODY_HANDLE` and
/// `$BODY_WAKE` are the only things a fixture varies.
///
/// The allocator wraps rather than growing: these fixtures serve many
/// dispatches and the engine copies each record in and the guest never retains
/// it, so a ring is both sufficient and keeps the dirty set of the density
/// fixture honest.
pub fn guest(pages: u32, extra_imports: &str, extra_funcs: &str, handle: &str, wake: &str) -> Vec<u8> {
    let heap_top = pages as usize * 65536 - 4096;
    let src = format!(
        r#"
(module
  {extra_imports}
  (memory (export "memory") {pages} {pages})
  (global $bump (export "g_bump") (mut i32) (i32.const 4096))
  (global $calls (export "g_calls") (mut i64) (i64.const 0))

  ;; (size, align) -> ptr. 0 means "cannot allocate", which the engine
  ;; classifies as TrapKind::OutOfMemory.
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

  ;; Burn `n` iterations of pure arithmetic: CPU with no memory traffic, so a
  ;; metering test is not also a dirty-page test.
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

  ;; Dirty `n` 4 KiB pages, one store per page, starting at 64 KiB.
  (func $dirty (param $n i32)
    (local $i i32)
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $n)))
      (i64.store (i32.add (i32.const 65536) (i32.mul (local.get $i) (i32.const 4096)))
                 (i64.const 3735928559))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l))))

  (func (export "probe") (param $a i32) (result i64) (i64.load (local.get $a)))
  (func (export "poke") (param $a i32) (param $v i64) (i64.store (local.get $a) (local.get $v)))
  {extra_funcs}
)
"#
    );
    wat::parse_str(&src).expect("fixture .wat must assemble")
}

/// `PollStatus::Done { response: 0 }` as the packed `i64` the ABI expects.
pub const DONE: &str = "(i64.const 0)";
/// `PollStatus::Pending`.
pub const PENDING: &str = "(i64.const 1)";

pub fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "wisp" "{name}" (func ${name} {params} {results}))"#)
}

// ---------------------------------------------------------------------------
// Test bindings
// ---------------------------------------------------------------------------

/// Records what the guest asked for, and hands back a coarse clock that really
/// does advance — so a frozen-clock test is testing the engine's freeze rather
/// than a host that happened to return the same number twice.
#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Arc<parking_lot::Mutex<Vec<String>>>,
    /// Incremented on every `slice_clock_ms()`, so consecutive *slices* always
    /// see different values.
    pub clock: Arc<AtomicU64>,
    pub clock_reads: Arc<AtomicU64>,
}

pub struct TestBindings {
    pub rec: Recorder,
    pub init_sandbox: bool,
    pub next_future: u64,
    pub fill_byte: u8,
    /// When false, `call_fill` denies — the default `HostBindings` behaviour.
    pub allow_fill: bool,
    /// What `call_fill` reports as the value's FULL length, which for
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
    pub fn init_sandbox(rec: Recorder) -> Self {
        Self { init_sandbox: true, ..Self::new(rec) }
    }
    fn note(&self, call: &ImportCall<'_>) {
        self.rec.calls.lock().push(format!("{}({} bytes)", call.import, call.payload.len()));
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
        // Vary per call, deliberately.
        //
        // A fake CSPRNG that answers the same bytes every time cannot tell a
        // real per-call draw from a cached one.
        out.fill(self.fill_byte);
        self.fill_byte = self.fill_byte.wrapping_add(1);
        // `(ptr, cap) -> full length`: the host may know more than it could fit.
        self.full_length.unwrap_or(out.len() as i64)
    }

    fn slice_clock_ms(&self) -> u64 {
        // Advances on every read, which is the strongest possible source for a
        // "the engine froze it" assertion.
        self.rec.clock_reads.fetch_add(1, Ordering::Relaxed);
        self.rec.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_init_sandbox(&self) -> bool {
        self.init_sandbox
    }
}

pub fn pct(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(((sorted.len() - 1) as f64) * q) as usize]
}

pub fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|r| r.trim().trim_end_matches(" kB").trim().parse().ok())
        .unwrap_or(0)
}

pub fn smaps_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|r| r.trim().trim_end_matches(" kB").trim().parse().ok())
        .unwrap_or(0)
}

/// A `DispatchRecord` with nothing interesting in it.
pub fn http_dispatch(id: u64) -> wisp_abi::dispatch::DispatchRecord {
    use wisp_abi::dispatch::*;
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/x".into(),
            path: "/x".into(),
            query: vec![],
            headers: vec![("host".into(), "example.test".into())],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 30_000,
        now_ms: 1_700_000_000_000,
    }
}
