//! The WASI PAL adapter exercised through the full engine: composition,
//! the quantized frozen clock, entropy delegation, and trap-on-call stubs.

mod common;

use common::*;
use wisp_engine::{Completion, CpuBudget, InstanceOps};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

/// A `DispatchRecord` with nothing interesting in it.
fn http_dispatch(id: u64) -> wisp_abi::dispatch::DispatchRecord {
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

/// A guest that imports `wasi:*` as well as `wisp`, with the `cabi_realloc`
/// export the canonical ABI needs to return a `list<u8>`.
fn pal_guest(handle: &str) -> Vec<u8> {
    let imports = String::from(
        r#"
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $wasi_now (result i64)))
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $wasi_random (param i64 i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $wasi_exit (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $wasi_env (param i32)))
"#,
    );
    let funcs = r#"
  (global $cabi (mut i32) (i32.const 40000))
  (func (export "cabi_realloc")
        (param $old i32) (param $oldsz i32) (param $align i32) (param $newsz i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $cabi))
    (global.set $cabi
      (i32.and (i32.add (i32.add (local.get $p) (local.get $newsz)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))
"#;
    guest(4, &imports, funcs, handle, DONE)
}

#[test]
fn a_module_with_wasi_imports_instantiates_through_the_pal_adapter() {
    let e = test_engine();
    let wasm = pal_guest("(i64.const 0)");
    let t = e
        .load_template(&wasm)
        .expect("a whitelisted wasi: surface must instantiate");

    assert!(t.uses_pal(), "the adapter was composed for this module");
    let composed: Vec<String> = t.pal_composed().iter().map(|d| d.full_name()).collect();
    assert_eq!(
        composed.len(),
        4,
        "every declared wasi: import was composed: {composed:?}"
    );

    let mut caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    caps.sort_unstable();
    assert_eq!(
        caps,
        vec!["clock_now_coarse", "entropy_fill"],
        "the adapter drew on exactly the two declared capabilities"
    );

    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the module dispatches"
    );
}

/// A nanosecond monotonic clock would pass "the guest can read the time" and
/// fail both assertions; a fine timer is the measurement primitive
/// Spectre-class attacks need.
#[test]
fn the_pal_clock_is_quantized_and_frozen_within_a_slice() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (call $wasi_now))
    (i64.store (i32.const 3008) (call $wasi_now))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let read = |i: &mut wisp_engine::Instance, off: usize| -> i64 {
        let m = i.memory();
        i64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
    };

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );
    let (a, b) = (read(&mut i, 3000), read(&mut i, 3008));
    assert_eq!(
        a, b,
        "the clock is frozen within a slice, so two reads cannot differ"
    );
    assert_eq!(
        a % 1_000_000,
        0,
        "the delegated clock is quantized to the coarse clock's millisecond: {a} ns"
    );

    assert_eq!(
        i.dispatch(&http_dispatch(2), CpuBudget::http()).completion,
        Completion::Done,
        "the second dispatch completes"
    );
    let c = read(&mut i, 3000);
    assert!(
        c > a,
        "the coarse clock must advance between slices ({a} then {c}), or the freeze test is \
         vacuous"
    );
    assert_eq!(
        c % 1_000_000,
        0,
        "quantization must survive the slice boundary"
    );
}

/// Entropy captured into a template, or memoised anywhere on the delegation
/// path, would be shared by every clone and look fine, because random-looking
/// bytes arrive either way.
#[test]
fn the_pal_entropy_delegation_draws_per_call_and_never_caches() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_random (i64.const 16) (i32.const 3000))
    (call $wasi_random (i64.const 16) (i32.const 3016))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (p1, l1, p2, l2) = {
        let m = i.memory();
        (
            read_u32(m, 3000) as usize,
            read_u32(m, 3004),
            read_u32(m, 3016) as usize,
            read_u32(m, 3020),
        )
    };
    assert_eq!(
        (l1, l2),
        (16, 16),
        "the adapter returns the requested length"
    );
    assert_ne!(
        p1, p2,
        "each draw allocates its own buffer through `cabi_realloc`"
    );

    let (first, second) = {
        let m = i.memory();
        (m[p1..p1 + 16].to_vec(), m[p2..p2 + 16].to_vec())
    };
    assert_ne!(
        first, second,
        "two draws in one dispatch returned identical bytes, so something on the delegation \
         path cached the CSPRNG output"
    );
}

/// A stub that returned zero would let the guest carry on, turning a denied
/// capability into a supported one that answers badly.
#[test]
fn a_pal_trap_on_call_stub_traps_rather_than_answering() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_exit (i32.const 0))
    (i64.store (i32.const 3000) (i64.const 1))   ;; must never run
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "a denied capability must not answer"
    );
    let msg = out.trap.map(|t| t.message).unwrap_or_default();
    assert!(
        msg.contains("wasi:cli/exit") && msg.contains("trap-on-call"),
        "the trap must name the import and say why: {msg}"
    );
    assert!(
        i.reset().is_err(),
        "a trapped instance is discarded, not reset"
    );
}

#[test]
fn the_pal_environment_delegation_answers_empty() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (i64.const -1))  ;; poison the return area
    (call $wasi_env (i32.const 3000))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (ptr, len) = {
        let m = i.memory();
        (read_u32(m, 3000), read_u32(m, 3004))
    };
    assert_eq!(
        (ptr, len),
        (0, 0),
        "there is no ambient environment, so the list is empty"
    );
}
