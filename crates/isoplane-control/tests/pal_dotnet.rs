use isoplane_abi::config::GuestMemoryBytes;
use isoplane_control::validator::{self, ViolationCode};
use isoplane_engine::IsoplaneEngine;

fn engine() -> IsoplaneEngine {
    IsoplaneEngine::new(isoplane_fixtures::engine_config(2)).expect("the test engine must start")
}

#[test]
fn the_mono_module_the_validator_admits_links_through_the_platform_adapter() {
    let wasm = isoplane_fixtures::mono_runtime();
    validator::validate_with_limits(
        &wasm,
        isoplane_abi::config::GuestMemoryBytes::default(),
        Some(GuestMemoryBytes::WASM32_MAX),
        &Default::default(),
    )
    .expect("the Mono fixture must satisfy admission");
    assert!(
        !platform_imports(&wasm).is_empty(),
        "Mono must exercise the platform adapter"
    );
    // Loading panics unless execution composes every platform import the module declares.
    isoplane_fixtures::load_compiled_runtime(&engine());
}

#[test]
fn an_undeclared_mono_platform_interface_is_refused_by_admission_and_execution() {
    let wasm = isoplane_fixtures::mono_runtime();
    let mutated = rename_import(&wasm, "wasi:io/poll@0.2.0", "wasi:io/poll@0.9.0");
    let violations = validator::validate_with_limits(
        &mutated,
        isoplane_abi::config::GuestMemoryBytes::default(),
        Some(GuestMemoryBytes::WASM32_MAX),
        &Default::default(),
    )
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

/// The imports of an admitted module that the platform table declares.
fn platform_imports(wasm: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let wasmparser::Payload::ImportSection(section) =
            payload.expect("an admitted module is well formed")
        else {
            continue;
        };
        for import in section.into_imports() {
            let import = import.expect("an admitted module's imports decode");
            if let Some(definition) = isoplane_abi::pal::lookup(import.module, import.name) {
                names.push(definition.full_name());
            }
        }
    }
    names
}

#[test]
fn only_the_debug_mono_module_carries_the_local_debug_marker() {
    assert_eq!(
        local_debug_markers(&isoplane_fixtures::mono_debug_runtime()),
        1,
        "the debug runtime must be linked with one {} section, which restricts it to a local debugger session",
        isoplane_abi::LOCAL_DEBUG_SECTION
    );
    assert_eq!(
        local_debug_markers(&isoplane_fixtures::mono_runtime()),
        0,
        "the production runtime must carry no {} section, which would keep it from being deployed",
        isoplane_abi::LOCAL_DEBUG_SECTION
    );
}

fn local_debug_markers(wasm: &[u8]) -> usize {
    wasmparser::Parser::new(0)
        .parse_all(wasm)
        .filter(|payload| {
            matches!(
                payload.as_ref().expect("the runtime module must parse"),
                wasmparser::Payload::CustomSection(section)
                    if section.name() == isoplane_abi::LOCAL_DEBUG_SECTION
            )
        })
        .count()
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
