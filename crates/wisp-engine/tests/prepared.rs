//! Prepared templates execute through the same activation/capture interface as portable input.
mod common;

use common::*;
use wisp_engine::prepared::{PreparedError, PreparedIdentity, PreparedStorage};
use wisp_engine::InstanceOps;

fn identity(wasm: &[u8]) -> PreparedIdentity {
    PreparedIdentity {
        input_hash: *blake3::hash(wasm).as_bytes(),
        runtime_profile: *blake3::hash(b"raw-wasm-test-profile").as_bytes(),
        initialization_hash: [3; 32],
    }
}

#[test]
fn prepared_state_matches_portable_state_and_survives_source_removal() {
    let engine = test_engine();
    let wasm = guest(
        4,
        "",
        r#"
        (data (i32.const 9000) "prepared memory")
        (global (export "counter") (mut i64) (i64.const 41))
        (table (export "table") 4 4 funcref)
        (elem (i32.const 0) $marker)
        (func $marker (result i32) i32.const 7)
    "#,
        DONE,
        DONE,
    );
    let expected = engine.load_template(&wasm).expect("portable template");
    let prepared = engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare");
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("version");
    prepared
        .publish(&path, PreparedStorage::VerifiedCopy)
        .expect("publish");
    let loaded = unsafe {
        // SAFETY: this test compiled the artifact into its own private fixture storage.
        engine.load_prepared(&path, identity(&wasm))
    }
    .expect("load");
    assert_eq!(
        loaded.memory_image_arc(),
        expected.memory_image_arc(),
        "prepared sparse state must preserve all initial memory"
    );
    std::fs::remove_dir_all(&path).expect("remove source files");
    let mut instance = activate(&engine, &loaded);
    let mut original = activate(&engine, &expected);
    assert_eq!(
        instance.memory(),
        original.memory(),
        "activation must retain prepared memory after artifact removal"
    );
    assert_eq!(
        postcard::to_allocvec(&instance.capture_mutable_state().expect("capture")).expect("encode"),
        postcard::to_allocvec(&original.capture_mutable_state().expect("capture")).expect("encode"),
        "prepared globals and tables must match portable initialization"
    );
}

#[test]
fn changed_payloads_are_rejected_when_the_manifest_is_unchanged() {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let directory = tempfile::tempdir().expect("directory");
    let prepared = engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare");
    for (index, name) in ["template.cwasm", "baseline.bin", "state.bin"]
        .iter()
        .enumerate()
    {
        let path = directory.path().join(index.to_string());
        prepared
            .publish(&path, PreparedStorage::VerifiedCopy)
            .expect("publish");
        let payload = path.join(name);
        let mut bytes = std::fs::read(&payload).expect("read");
        bytes[0] ^= 1;
        std::fs::write(payload, bytes).expect("alter payload");
        assert!(
            matches!(
                unsafe {
                    // SAFETY: this test compiled the artifact into its own private fixture storage.
                    engine.load_prepared(&path, identity(&wasm))
                },
                Err(PreparedError::Invalid(_))
            ),
            "changed {name} must be rejected before native execution"
        );
    }
}

#[test]
fn an_unexpected_runtime_profile_is_rejected() {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare")
        .publish(&path, PreparedStorage::VerifiedCopy)
        .expect("publish");
    let mut other = identity(&wasm);
    other.runtime_profile[0] ^= 1;
    assert!(
        matches!(
            unsafe {
                // SAFETY: this test compiled the artifact into its own private fixture storage.
                engine.load_prepared(&path, other)
            },
            Err(PreparedError::Incompatible(_))
        ),
        "runtime compatibility must be checked before native deserialization"
    );
}

#[test]
fn another_initialization_context_cannot_substitute_a_prepared_template() {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare")
        .publish(&path, PreparedStorage::VerifiedCopy)
        .expect("publish");
    let mut other = identity(&wasm);
    other.initialization_hash[0] ^= 1;
    assert!(
        matches!(
            unsafe {
                // SAFETY: this test compiled the artifact into its own private fixture storage.
                engine.load_prepared(&path, other)
            },
            Err(PreparedError::Incompatible(_))
        ),
        "identical source/runtime must not admit another initialization configuration"
    );
}

#[test]
fn prepared_portable_input_cannot_rerun_a_start_function_on_activation() {
    let engine = test_engine();
    let wasm =
        wat::parse_str("(module (memory (export \"memory\") 1) (func $start) (start $start))")
            .expect("wasm");
    assert!(
        matches!(
            engine.prepare_template(&wasm, identity(&wasm)),
            Err(PreparedError::Invalid(_))
        ),
        "preparation must bake and remove the start function before native serialization"
    );
}
