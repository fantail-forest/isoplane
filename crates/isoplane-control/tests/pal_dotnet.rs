//! The retained Mono runtime through both capability admission boundaries.

use isoplane_abi::config::Limits;
use isoplane_control::validator::{self, ViolationCode};
use isoplane_engine::{EngineConfig, IsoplaneEngine};

fn engine() -> IsoplaneEngine {
    IsoplaneEngine::new(EngineConfig {
        max_instances: 2,
        max_memory_bytes: 256 << 20,
        ..EngineConfig::default()
    })
    .expect("the test engine must start")
}

#[test]
fn the_mono_module_links_exactly_the_platform_imports_admitted_by_the_validator() {
    let wasm = isoplane_fixtures::mono_runtime();
    let report = validator::validate(&wasm, Limits::default())
        .expect("the Mono fixture must satisfy admission");
    let template = engine()
        .load_template(&wasm)
        .expect("the admitted Mono fixture must load into the engine");
    let mut admitted: Vec<_> = report.pal.iter().map(|import| import.full_name()).collect();
    let mut linked: Vec<_> = template
        .pal_composed()
        .iter()
        .map(|import| import.full_name())
        .collect();
    admitted.sort();
    linked.sort();
    assert!(
        !admitted.is_empty(),
        "Mono must exercise the platform adapter"
    );
    assert_eq!(
        linked, admitted,
        "execution must compose every admitted platform import"
    );
}

#[test]
fn an_undeclared_mono_platform_interface_is_refused_by_admission_and_execution() {
    let wasm = isoplane_fixtures::mono_runtime();
    let mutated = rename_import(&wasm, "wasi:io/poll@0.2.0", "wasi:io/poll@0.9.0");
    let violations = validator::validate(&mutated, Limits::default())
        .expect_err("an unknown platform interface must fail admission");
    assert!(
        violations
            .iter()
            .any(|violation| violation.code == ViolationCode::V1UndeclaredImport),
        "the changed platform interface must be rejected as undeclared: {violations:?}"
    );
    assert!(
        engine().load_template(&mutated).is_err(),
        "execution must also refuse the undeclared platform interface"
    );
}

fn rename_import(wasm: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(
        from.len(),
        to.len(),
        "the rewrite must preserve every LEB length"
    );
    let (from, to) = (from.as_bytes(), to.as_bytes());
    let mut out = wasm.to_vec();
    let mut hits = 0;
    let mut i = 0;
    while i + from.len() <= out.len() {
        if &out[i..i + from.len()] == from {
            out[i..i + from.len()].copy_from_slice(to);
            hits += 1;
            i += from.len();
        } else {
            i += 1;
        }
    }
    assert!(
        hits > 0,
        "`{}` not found in the module",
        String::from_utf8_lossy(from)
    );
    out
}
