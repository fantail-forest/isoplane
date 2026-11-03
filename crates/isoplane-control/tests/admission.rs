//! Local admission enforces sandbox capabilities and operator-supplied limits.

use isoplane_abi::config::AppManifest;
use isoplane_control::admission::{admit, admit_shared_runtime};
use isoplane_control::AdmissionError;

fn app_wat() -> &'static str {
    r#"(module
      (import "isoplane" "service_call" (func (param i32 i32) (result i64)))
      (import "isoplane" "http_fetch" (func (param i32 i32) (result i64)))
      (memory (export "memory") 256 2048)
      (func (export "isoplane_initialize") (result i64) i64.const 0)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 0)
      (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 1)
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 1))"#
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        guest_memory_bytes: None,
        resource_limits: Default::default(),
        validation_limits: Default::default(),
        name: name.into(),
        bindings: Default::default(),
        init_visible_vars: vec![("REGION".into(), "eu-west".into())],
    }
}

#[test]
fn admission_records_capabilities_and_the_initialization_entry() {
    let wasm = wat::parse_str(app_wat()).unwrap();
    let report = admit(&manifest("shop"), &wasm).unwrap();
    assert!(report.has_initialize());
    assert!(report.capabilities.contains(&"service_call"));
    assert!(report.capabilities.contains(&"http_fetch"));
}

#[test]
fn an_undeclared_import_is_rejected_by_both_admission_paths() {
    let wasm = wat::parse_str(app_wat().replace(
        "\"isoplane\" \"service_call\"",
        "\"wasi_snapshot_preview1\" \"fd_write\"",
    ))
    .unwrap();
    let manifest = manifest("rejected");
    for result in [
        admit(&manifest, &wasm),
        admit_shared_runtime(&manifest, &wasm),
    ] {
        assert!(matches!(result, Err(AdmissionError::Rejected(violations))
            if violations.iter().any(|v| v.code == isoplane_control::validator::ViolationCode::V1UndeclaredImport)));
    }
}

#[test]
fn a_module_without_an_initializer_is_admitted_without_inventing_one() {
    let wasm = wat::parse_str(
        r#"(module
      (memory (export "memory") 17 2048)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 0)
      (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 1)
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 1))"#,
    )
    .unwrap();
    let report = admit(&manifest("tiny"), &wasm).unwrap();
    assert!(!report.has_initialize());
    assert!(report.capabilities.is_empty());
}

#[test]
fn the_compressed_module_budget_accepts_the_exact_boundary() {
    let wasm = wat::parse_str(app_wat()).unwrap();
    let size = zstd::encode_all(wasm.as_slice(), 19).unwrap().len() as u64;
    let mut app = manifest("bounded");
    app.resource_limits.module_bytes_compressed = Some(size - 1);
    assert!(
        matches!(admit(&app, &wasm), Err(AdmissionError::TooLarge { size: actual, limit })
        if actual == size && limit == size - 1)
    );
    app.resource_limits.module_bytes_compressed = Some(size);
    assert!(admit(&app, &wasm).is_ok());
}

#[test]
fn shared_runtime_admission_allows_wasm32_memory_without_charging_its_size_to_the_app() {
    let wasm = wat::parse_str(app_wat().replace("256 2048", "256 65536")).unwrap();
    let mut app = manifest("shared");
    app.resource_limits.module_bytes_compressed = Some(1);
    assert!(admit_shared_runtime(&app, &wasm).is_ok());
    assert!(
        matches!(admit(&app, &wasm), Err(AdmissionError::Rejected(violations))
        if violations.iter().any(|v| v.code == isoplane_control::validator::ViolationCode::V3MemoryShape))
    );
    app.validation_limits.max_functions = 1;
    assert!(matches!(
        admit_shared_runtime(&app, &wasm),
        Err(AdmissionError::Rejected(_))
    ));
}
