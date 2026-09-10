//! The `wasi:http` entries of the WASI PAL adapter corpus, and rule V1
//! around them: the runtime's own HTTP client reaches the host's outbound
//! HTTP capability through the adapter, and nothing beside it.

use wisp_abi::config::Limits;
use wisp_control::validator::{self, ViolationCode as V};

#[path = "common/validate.rs"]
mod validate;
use validate::{assert_violates, parse, REQUIRED_EXPORTS};

/// A module shaped like the runtime's `WasiHttpHandler` leaves it: the
/// client's `wasi:http` calls plus the body stream read, and one of the
/// request-option setters the client never calls, which traps.
fn http_wat() -> String {
    format!(
        r#"(module
  (import "wisp" "log_emit" (func $log (param i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[static]fields.from-list" (func $fields (param i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[constructor]outgoing-request" (func $request (param i32) (result i32)))
  (import "wasi:http/outgoing-handler@0.2.0" "handle" (func $handle (param i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]future-incoming-response.get" (func $get (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.read" (func $read (param i32 i64 i32)))
  (import "wasi:http/types@0.2.0" "[method]request-options.set-connect-timeout" (func $timeout (param i32 i32 i64) (result i32)))
  {REQUIRED_EXPORTS}
)"#
    )
}

/// **The rule.** The client surface is whitelisted and delegated, and the
/// manifest names the capability the module reaches through it: outbound
/// HTTP, so a tenant reading the manifest sees egress.
#[test]
fn v1_accepts_the_runtimes_http_client_surface() {
    let wasm = parse(&http_wat());
    let report = validator::validate(&wasm, Limits::default())
        .unwrap_or_else(|vs| panic!("the runtime's wasi:http surface must validate: {vs:?}"));

    assert_eq!(report.pal.len(), 6, "every wasi import is composed");
    let mut delegated: Vec<&str> = report
        .pal
        .iter()
        .filter(|p| p.disposition.is_delegated())
        .map(|p| p.func)
        .collect();
    delegated.sort_unstable();
    assert_eq!(
        delegated,
        vec![
            "[constructor]outgoing-request",
            "[method]future-incoming-response.get",
            "[method]input-stream.read",
            "[method]request-options.set-connect-timeout",
            "[static]fields.from-list",
            "handle",
        ],
        "each entry is delegated with its own body, the unsupported setter to a trap"
    );
    assert!(
        report.capabilities.contains(&"http_call"),
        "reaching wasi:http through the adapter is holding the outbound HTTP capability"
    );
    assert!(
        report.capabilities.contains(&"socket_call"),
        "the body stream is a socket-capability stream"
    );

    let manifest = report.manifest();
    println!("{manifest}");
    assert!(
        manifest.contains("wasi:http/outgoing-handler@0.2.0::handle -> delegates to `http_call`")
    );
    assert!(
        manifest.contains("egress:http"),
        "the manifest must class the capability as egress"
    );
}

/// **The negative control.** The whitelist is keyed by function: another
/// function of `wasi:http/types` is not admitted by admitting these, nor is
/// another version of the interface.
#[test]
fn v1_rejects_an_http_import_outside_the_whitelist() {
    assert_violates(
        "same-interface-different-function",
        &http_wat().replace(
            r#""[static]fields.from-list" (func $fields (param i32 i32 i32))"#,
            r#""[static]fields.from-map" (func $fields (param i32 i32 i32))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    assert_violates(
        "wasi-http-0.3.0",
        &http_wat().replace(
            "wasi:http/outgoing-handler@0.2.0",
            "wasi:http/outgoing-handler@0.3.0",
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
}

/// A delegated `wasi:http` import runs against its declared type, so its
/// shape is checked like any other; a trap-on-call entry is checked too,
/// since it is delegated to the trap with a signature.
#[test]
fn v1_checks_the_signature_of_a_delegated_http_import() {
    assert_violates(
        "handle-wrong-signature",
        &http_wat().replace(
            r#""handle" (func $handle (param i32 i32 i32 i32))"#,
            r#""handle" (func $handle (param i32 i32 i32 i32) (result i32))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
    assert_violates(
        "read-wrong-signature",
        &http_wat().replace(
            r#""[method]input-stream.read" (func $read (param i32 i64 i32))"#,
            r#""[method]input-stream.read" (func $read (param i32 i32 i32))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
}
