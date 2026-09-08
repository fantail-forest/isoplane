//! The WASI PAL adapter corpus, and rule V1 around it.

use wisp_abi::config::Limits;
use wisp_control::validator::{self, ViolationCode as V};

#[path = "common/validate.rs"]
mod validate;
use validate::{assert_violates, codes, parse, REQUIRED_EXPORTS};

/// A module shaped like what NativeAOT-LLVM emits: a handful of `wisp`
/// imports plus the `wasi:*` residue the toolchain leaves behind. The four
/// `wasi` entries here are the ones the adapter delegates; `output-stream.blocking-flush`
/// stands in for the trap-on-call remainder.
fn wasi_wat() -> String {
    format!(
        r#"(module
  (import "wisp" "log_emit" (func $log (param i32 i32 i32)))
  (import "wasi:cli/environment@0.2.0"    "get-environment"  (func $env  (param i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "now"          (func $now  (result i64)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "resolution"   (func $res  (result i64)))
  (import "wasi:random/random@0.2.0"      "get-random-bytes" (func $rand (param i64 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))
  {REQUIRED_EXPORTS}
)"#
    )
}

/// **The rule.** A module whose non-`wisp` imports are all in the enumerated
/// whitelist deploys — that is the whole point of the adapter, and the
/// thing that lets a stock .NET module pass V1 without an exemption.
#[test]
fn v1_accepts_a_module_whose_wasi_imports_are_all_whitelisted() {
    let wasm = parse(&wasi_wat());
    let report = validator::validate(&wasm, Limits::default())
        .unwrap_or_else(|vs| panic!("whitelisted wasi imports must validate: {vs:?}"));

    // Five wasi imports composed: four delegated, one trap-on-call.
    assert_eq!(report.pal.len(), 5);
    assert_eq!(
        report
            .pal
            .iter()
            .filter(|p| p.disposition.is_delegated())
            .count(),
        4
    );

    // **The manifest must stay honest.** This module imports neither
    // `clock_now_coarse` nor `entropy_fill` from `wisp`, but it reaches both
    // through the adapter — so it holds both capabilities and the manifest
    // says so. A delegated import that vanished from the report would be
    // invariant 4 defeated by a composition step.
    assert!(report.capabilities.contains(&"clock_now_coarse"));
    assert!(report.capabilities.contains(&"entropy_fill"));

    let manifest = report.manifest();
    println!("{manifest}");
    assert!(manifest.contains("wasi-pal"));
    assert!(manifest.contains("wasi:random/random@0.2.0::get-random-bytes"));
    assert!(manifest.contains("delegates to `entropy_fill`"));
    assert!(
        manifest.contains("answers empty"),
        "the environment delegation must be visible"
    );
    assert!(manifest.contains("trap-on-call"));
    assert!(manifest.contains("wasi:io/streams@0.2.0"));
}

/// **The negative control for that rule.** The whitelist is keyed by
/// `(interface, function)`, not by interface — otherwise admitting
/// `wasi:random/random` for `get-random-bytes` would silently admit every
/// other function in the same interface, and an enumerated whitelist that
/// enumerates interfaces is not an enumerated whitelist.
#[test]
fn v1_rejects_a_wasi_import_outside_the_whitelist() {
    // A different function in an interface we DO admit.
    assert_violates(
        "same-interface-different-function",
        &wasi_wat().replace(
            r#""get-random-bytes" (func $rand (param i64 i32))"#,
            r#""get-insecure-random-bytes" (func $rand (param i64 i32))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    // A near-miss spelling of an admitted function.
    assert_violates(
        "near-miss-name",
        &wasi_wat().replace(
            r#""now"          (func $now  (result i64))"#,
            r#""now-fine"     (func $now  (result i64))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    // A different WASI generation entirely. preview1 is not preview2, and the
    // whitelist is versioned because the ABI is.
    assert_violates(
        "wasi-preview1",
        &wasi_wat().replace(
            r#"(import "wasi:cli/environment@0.2.0"    "get-environment"  (func $env  (param i32)))"#,
            r#"(import "wasi_snapshot_preview1" "fd_write" (func $env (param i32)))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    // A future WASI version of an admitted interface. The adapter implements
    // the 0.2.0 canonical-ABI lowering; 0.3.0 is a different contract.
    assert_violates(
        "wasi-0.3.0",
        &wasi_wat().replace("wasi:random/random@0.2.0", "wasi:random/random@0.3.0"),
        Limits::default(),
        V::V1UndeclaredImport,
    );
}

/// A *delegated* import gets a real body run against its declared type, so
/// its shape is checked like any `wisp` import. Half a type check is not a
/// type check — and here it would be a type confusion in host code.
#[test]
fn v1_checks_the_signature_of_a_delegated_wasi_import() {
    assert_violates(
        "delegated-wrong-signature",
        &wasi_wat().replace(
            r#""get-random-bytes" (func $rand (param i64 i32))"#,
            r#""get-random-bytes" (func $rand (param i32 i32))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
    assert_violates(
        "delegated-clock-wrong-signature",
        &wasi_wat().replace(
            r#""now"          (func $now  (result i64))"#,
            r#""now"          (func $now  (result i32))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
}

/// **The negative control for the signature rule.** A trap-on-call stub
/// is generated from the module's own declared type and traps before reading
/// an argument, so its shape is genuinely irrelevant — pinning one would be a
/// check that breaks the first time a toolchain lowers the same function
/// differently, for no security gain. This test exists so that the asymmetry
/// is deliberate rather than an oversight.
#[test]
fn v1_does_not_pin_the_signature_of_a_trap_on_call_stub() {
    let wat = wasi_wat().replace(
        r#""[method]output-stream.blocking-flush" (func $flush (param i32 i32))"#,
        r#""[method]output-stream.blocking-flush" (func $flush (param i32 i32 i32 i32) (result i32))"#,
    );
    let report = validator::validate(&parse(&wat), Limits::default())
        .expect("a trap-on-call stub is shape-agnostic");
    assert_eq!(report.pal.len(), 5);
}

/// Importing a *memory* under a whitelisted WASI name is not a function
/// import and must not slip through the composition path (no host
/// objects inside the sandbox).
#[test]
fn v1_rejects_a_non_function_wasi_import() {
    let wat = wasi_wat().replace(
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" "now"          (func $now  (result i64)))"#,
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" "now" (global i64))"#,
    );
    let got = codes(&parse(&wat), Limits::default());
    assert!(got.contains(&V::V1SignatureMismatch), "got {got:?}");
}

#[test]
fn a_native_socket_delegation_requires_its_full_canonical_signature() {
    assert_violates(
        "native-socket-wrong-signature",
        &wasi_wat().replace(
            r#"(import "wasi:io/streams@0.2.0" "[method]output-stream.blocking-flush" (func $flush (param i32 i32)))"#,
            r#"(import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-connect" (func $c (param i32 i32)))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
}
