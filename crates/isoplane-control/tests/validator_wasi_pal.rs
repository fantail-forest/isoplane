use isoplane_control::validator::ViolationCode as V;

#[path = "common/validate.rs"]
mod validate;
use validate::{assert_violates, codes, parse, validate, REQUIRED_EXPORTS};

/// A module with the canonical WASI imports the shared runtime declares
/// beside its `isoplane` imports.
fn wasi_wat() -> String {
    format!(
        r#"(module
  (import "isoplane" "log_emit" (func $log (param i32 i32 i32)))
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $timer (param i64) (result i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func $poll (param i32 i32 i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop (param i32)))
  {REQUIRED_EXPORTS}
)"#
    )
}

#[test]
fn v1_accepts_a_module_whose_wasi_imports_are_all_whitelisted() {
    let wasm = parse(&wasi_wat());
    validate(&wasm, isoplane_abi::config::GuestMemoryBytes::default())
        .unwrap_or_else(|vs| panic!("whitelisted wasi imports must validate: {vs:?}"));
}

/// The whitelist is keyed by `(interface, function)`: keyed by interface,
/// admitting `wasi:io/poll` for `poll` would admit every other function in it.
#[test]
fn v1_rejects_a_wasi_import_outside_the_whitelist() {
    // A different function in an interface we DO admit: a wait inside a call.
    assert_violates(
        "same-interface-different-function",
        &wasi_wat().replace(
            r#""poll" (func $poll (param i32 i32 i32))"#,
            r#""[method]pollable.block" (func $poll (param i32))"#,
        ),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1UndeclaredImport,
    );
    // A near-miss spelling of an admitted function.
    assert_violates(
        "near-miss-name",
        &wasi_wat().replace(
            r#""subscribe-duration" (func $timer (param i64) (result i32))"#,
            r#""subscribe-durations" (func $timer (param i64) (result i32))"#,
        ),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1UndeclaredImport,
    );
    // A different WASI generation entirely. preview1 is not preview2, and the
    // whitelist is versioned because the ABI is.
    assert_violates(
        "wasi-preview1",
        &wasi_wat().replace(
            r#"(import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func $drop (param i32)))"#,
            r#"(import "wasi_snapshot_preview1" "fd_write" (func $drop (param i32)))"#,
        ),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1UndeclaredImport,
    );
    // A future WASI version of an admitted interface. The adapter implements
    // the 0.2.0 canonical-ABI lowering; 0.3.0 is a different contract.
    assert_violates(
        "wasi-0.3.0",
        &wasi_wat().replace("wasi:io/poll@0.2.0", "wasi:io/poll@0.3.0"),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1UndeclaredImport,
    );
}

/// A WASI import gets a real body run against its declared type, so an
/// unchecked signature would be a type confusion in host code.
#[test]
fn v1_checks_the_signature_of_a_delegated_wasi_import() {
    assert_violates(
        "delegated-wrong-parameter",
        &wasi_wat().replace(
            r#""subscribe-duration" (func $timer (param i64) (result i32))"#,
            r#""subscribe-duration" (func $timer (param i32) (result i32))"#,
        ),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1SignatureMismatch,
    );
    assert_violates(
        "delegated-wrong-result",
        &wasi_wat().replace(
            r#""poll" (func $poll (param i32 i32 i32))"#,
            r#""poll" (func $poll (param i32 i32 i32) (result i32))"#,
        ),
        isoplane_abi::config::GuestMemoryBytes::default(),
        V::V1SignatureMismatch,
    );
}

/// Importing a *global* under a whitelisted WASI name is not a function
/// import and must not slip through the composition path (no host
/// objects inside the sandbox).
#[test]
fn v1_rejects_a_non_function_wasi_import() {
    let wat = wasi_wat().replace(
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $timer (param i64) (result i32)))"#,
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (global i64))"#,
    );
    let got = codes(
        &parse(&wat),
        isoplane_abi::config::GuestMemoryBytes::default(),
    );
    assert!(got.contains(&V::V1SignatureMismatch), "got {got:?}");
}
