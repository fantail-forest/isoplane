//! Prepared templates execute through the same activation/capture interface as portable input.
mod common;
use isoplane_test_support::published_files;

use common::*;
use isoplane_engine::prepared::{PreparedError, PreparedIdentity};
use isoplane_engine::InstanceOps;

/// Maps a template published at `path`.
fn load(
    engine: &isoplane_engine::IsoplaneEngine,
    path: &std::path::Path,
    expected: PreparedIdentity,
) -> Result<isoplane_engine::Template, PreparedError> {
    // SAFETY: callers use this test fixture's local compiler output and private storage;
    // they change files only before loading to check refusal, and retain mapped backing.
    unsafe { engine.load_prepared(path, expected, &[]) }
}

/// Maps a locally compiled fixture selected by its receipt.
fn restore(
    engine: &isoplane_engine::IsoplaneEngine,
    path: &std::path::Path,
    manifest_sha256: [u8; 32],
    runtime_profile: [u8; 32],
) -> Result<isoplane_engine::Template, PreparedError> {
    // SAFETY: as for `load`.
    unsafe { engine.restore_prepared(path, manifest_sha256, runtime_profile, &[]) }
}

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
    let directory = published_files::tempdir().expect("directory");
    let path = directory.path().join("version");
    prepared.publish(&path).expect("publish");
    let loaded = load(&engine, &path, identity(&wasm)).expect("load");
    assert_eq!(
        loaded.memory_image_arc(),
        expected.memory_image_arc(),
        "prepared sparse state must preserve all initial memory"
    );
    std::fs::remove_dir_all(&path).expect("remove source files");
    assert!(
        !path.exists(),
        "the original publication must no longer resolve"
    );
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
    let directory = published_files::tempdir().expect("directory");
    let prepared = engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare");
    for (index, name) in ["template.cwasm", "baseline.bin", "state.bin"]
        .iter()
        .enumerate()
    {
        let path = directory.path().join(index.to_string());
        prepared.publish(&path).expect("publish");
        tamper(&path.join(name));
        assert!(
            matches!(
                load(&engine, &path, identity(&wasm)),
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
    let directory = published_files::tempdir().expect("directory");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare")
        .publish(&path)
        .expect("publish");
    let mut other = identity(&wasm);
    other.runtime_profile[0] ^= 1;
    assert!(
        matches!(
            load(&engine, &path, other),
            Err(PreparedError::Incompatible(_))
        ),
        "runtime compatibility must be checked before native deserialization"
    );
}

#[test]
fn another_initialization_context_cannot_substitute_a_prepared_template() {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let directory = published_files::tempdir().expect("directory");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, identity(&wasm))
        .expect("prepare")
        .publish(&path)
        .expect("publish");
    let mut other = identity(&wasm);
    other.initialization_hash[0] ^= 1;
    assert!(
        matches!(
            load(&engine, &path, other),
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

fn compiled_artifact() -> (
    isoplane_engine::IsoplaneEngine,
    published_files::Directory,
    PreparedIdentity,
) {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let expected = identity(&wasm);
    let directory = published_files::tempdir().expect("compiled artifact");
    engine
        .prepare_template(&wasm, expected)
        .expect("prepare")
        .publish(&directory.path().join("version"))
        .expect("publish");
    (engine, directory, expected)
}

#[test]
fn receipt_restoration_checks_the_exact_manifest_and_runtime() {
    let (engine, directory, expected) = compiled_artifact();
    let path = directory.path().join("version");
    let digest = engine
        .prepared_manifest_sha256(&path)
        .expect("content receipt");
    let restored = restore(&engine, &path, digest, expected.runtime_profile)
        .expect("the receipt must select compiled state without input hashes supplied separately");
    assert!(
        activate(&engine, &restored).memory().len() >= 4 * 65536,
        "receipt restoration must produce an activatable template"
    );
}

#[test]
fn receipt_restoration_rejects_a_different_manifest_digest() {
    let (engine, directory, expected) = compiled_artifact();
    assert!(
        matches!(
            restore(
                &engine,
                &directory.path().join("version"),
                [0; 32],
                expected.runtime_profile
            ),
            Err(PreparedError::Incompatible("catalog manifest digest"))
        ),
        "an artifact selected by a different receipt must not execute"
    );
}

#[test]
fn receipt_restoration_rejects_a_different_runtime_profile() {
    let (engine, directory, _) = compiled_artifact();
    let path = directory.path().join("version");
    let digest = engine.prepared_manifest_sha256(&path).expect("receipt");
    assert!(
        matches!(
            restore(&engine, &path, digest, [0; 32]),
            Err(PreparedError::Incompatible("catalog runtime profile"))
        ),
        "the selected runtime must match the compiled prepared state"
    );
}

/// Maps a locally compiled fixture selected by its manifest digest.
fn restore_by_digest(
    engine: &isoplane_engine::IsoplaneEngine,
    path: &std::path::Path,
    manifest_sha256: [u8; 32],
    expected: PreparedIdentity,
) -> Result<isoplane_engine::Template, PreparedError> {
    // SAFETY: as for `load`.
    unsafe { engine.restore_prepared_by_digest(path, manifest_sha256, expected) }
}

#[test]
fn a_receipt_digest_selects_the_compiled_artifact() {
    let (engine, directory, expected) = compiled_artifact();
    let path = directory.path().join("version");
    let digest = engine.prepared_manifest_sha256(&path).expect("receipt");
    let restored = restore_by_digest(&engine, &path, digest, expected)
        .expect("the receipt digest must select the locally compiled artifact");
    assert!(
        activate(&engine, &restored).memory().len() >= 4 * 65536,
        "restoration by digest must produce an activatable template"
    );
}

#[test]
fn a_receipt_digest_refuses_a_substituted_manifest() {
    let (engine, directory, expected) = compiled_artifact();
    let path = directory.path().join("version");
    let digest = engine.prepared_manifest_sha256(&path).expect("receipt");
    let other = directory.path().join("other");
    engine
        .prepare_template(&guest(5, "", "", DONE, DONE), expected)
        .expect("prepare")
        .publish(&other)
        .expect("publish");
    let manifest = path.join("manifest.bin");
    std::fs::remove_file(&manifest).expect("remove the read-only manifest");
    std::fs::copy(other.join("manifest.bin"), &manifest).expect("substitute the manifest");
    assert!(
        matches!(
            restore_by_digest(&engine, &path, digest, expected),
            Err(PreparedError::Incompatible("catalog manifest digest"))
        ),
        "a manifest other than the selected receipt must not load"
    );
}

#[test]
fn a_receipt_digest_refuses_an_artifact_for_another_identity() {
    let (engine, directory, expected) = compiled_artifact();
    let path = directory.path().join("version");
    let digest = engine.prepared_manifest_sha256(&path).expect("receipt");
    let other = PreparedIdentity {
        initialization_hash: [4; 32],
        ..expected
    };
    assert!(
        matches!(
            restore_by_digest(&engine, &path, digest, other),
            Err(PreparedError::Incompatible(_))
        ),
        "a compiled artifact must not load for a different preparation identity"
    );
}

#[test]
fn builder_output_cannot_replace_host_platform_files() {
    let producer = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let declared = isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
        path: "/etc/ssl/certs/ca-certificates.crt".into(),
        bytes: b"roots".as_slice().into(),
        modified_ns: 42,
    }])
    .expect("declared files");
    let prepared = producer
        .prepare_template_with_files(&wasm, identity(&wasm), declared.clone())
        .expect("producer without reserved paths");
    let receiver = isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
        platform_files: declared,
        ..small_engine_config()
    })
    .expect("host with reserved trust path");
    let bytes = prepared.encode_builder_output().expect("trusted output");
    // SAFETY: the trusted engine above produced this serialized native code.
    assert!(
        unsafe { receiver.decode_builder_output(&bytes, identity(&wasm)) }.is_err(),
        "builder output cannot replace host-managed trust material before publication"
    );
}

#[test]
fn receipt_checks_do_not_read_native_or_baseline_payloads() {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let identity = identity(&wasm);
    let directory = published_files::tempdir().expect("directory");
    let path = directory.path().join("prepared");
    engine
        .prepare_template(&wasm, identity)
        .expect("prepare")
        .publish(&path)
        .expect("publish");
    let digest = engine.prepared_manifest_sha256(&path).expect("receipt");
    for name in ["template.cwasm", "baseline.bin", "state.bin"] {
        std::fs::remove_file(path.join(name)).expect("remove payload");
    }
    assert_eq!(
        engine
            .check_prepared_receipt(&path, digest, identity.runtime_profile)
            .expect("manifest-only receipt check"),
        identity,
        "receipt checks must not materialize native code or baseline pages"
    );
    assert!(
        restore(&engine, &path, digest, identity.runtime_profile).is_err(),
        "a manifest receipt alone must not qualify missing executable payloads"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn profiling_an_existing_template_emits_native_symbols_without_rebuilding_it() {
    use isoplane_test_support::{run_successfully, ChildTest};

    const CHILD: ChildTest =
        ChildTest::new("profiling_an_existing_template_emits_native_symbols_without_rebuilding_it");
    if CHILD.child_value().is_none() {
        let directory = tempfile::tempdir().expect("owned profiler output directory");
        run_successfully(
            CHILD.command("1").current_dir(directory.path()),
            "profile in an isolated process",
        );
        return;
    }
    let engine = test_engine();
    let wasm = guest(
        4,
        "",
        "(func $profile_marker (export \"profile_marker\") (result i32) i32.const 47)",
        DONE,
        DONE,
    );
    let expected = identity(&wasm);
    let directory = published_files::tempdir().expect("prepared artifact");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, expected)
        .expect("prepare without profiling")
        .publish(&path)
        .expect("publish");
    let dump = std::path::PathBuf::from(format!("jit-{}.dump", std::process::id()));
    assert!(
        !dump.exists(),
        "normal engine operation must not enable profiling output"
    );
    let profiled = isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
        perf_jitdump: true,
        ..small_engine_config()
    })
    .expect("profiled engine");
    let template = load(&profiled, &path, expected)
        .expect("profiling must accept the existing unprofiled artifact identity");
    assert_eq!(
        activate(&profiled, &template).memory().len(),
        4 * 65536,
        "profiled loading must preserve activatable guest memory"
    );
    let bytes = std::fs::read(dump).expect("jitdump file");
    let marker = b"profile_marker";
    assert!(
        bytes.windows(marker.len()).any(|window| window == marker),
        "loading existing native code must register its function symbols with perf"
    );
}

#[test]
fn preparation_settings_do_not_enable_execution_process_profiling() {
    let config = isoplane_engine::EngineConfig {
        perf_jitdump: true,
        ..small_engine_config()
    };
    let bytes = postcard::to_allocvec(&config).expect("serialize preparation settings");
    let restored: isoplane_engine::EngineConfig =
        postcard::from_bytes(&bytes).expect("deserialize preparation settings");
    assert!(
        !restored.perf_jitdump,
        "execution profiling must not make preparation write diagnostic files"
    );
    assert_eq!(
        restored.max_memory_bytes, config.max_memory_bytes,
        "excluding process-local diagnostics must preserve preparation memory limits"
    );
}

/// Publishes `wasm` prepared under `identity` as a mapped artifact at `path`.
fn publish_mapped(
    engine: &isoplane_engine::IsoplaneEngine,
    wasm: &[u8],
    identity: PreparedIdentity,
    path: &std::path::Path,
) {
    engine
        .prepare_template(wasm, identity)
        .expect("prepare")
        .publish(path)
        .expect("publish");
}

/// Flips the first byte of a published payload, keeping its length.
fn tamper(payload: &std::path::Path) {
    let mut bytes = std::fs::read(payload).expect("read payload");
    bytes[0] ^= 1;
    std::fs::remove_file(payload).expect("remove read-only payload");
    std::fs::write(payload, bytes).expect("write changed payload");
}

/// Two artifacts of one module whose manifests record the same native
/// payload under different initialization identities.
fn sharing_artifacts(
    engine: &isoplane_engine::IsoplaneEngine,
    directory: &std::path::Path,
) -> (Vec<u8>, [(std::path::PathBuf, PreparedIdentity); 2]) {
    let wasm = guest(
        4,
        "",
        r#"(data (i32.const 9000) "shared native")"#,
        DONE,
        DONE,
    );
    let first = identity(&wasm);
    let second = PreparedIdentity {
        initialization_hash: [4; 32],
        ..first
    };
    let artifacts = [
        (directory.join("first"), first),
        (directory.join("second"), second),
    ];
    for (path, identity) in &artifacts {
        publish_mapped(engine, &wasm, *identity, path);
    }
    (wasm, artifacts)
}

#[test]
fn a_template_shares_native_code_its_engine_already_holds_without_reading_its_own_copy() {
    let engine = test_engine();
    let directory = published_files::tempdir().expect("directory");
    let (wasm, [(first, first_identity), (second, second_identity)]) =
        sharing_artifacts(&engine, directory.path());
    tamper(&second.join("template.cwasm"));
    let held = load(&engine, &first, first_identity).expect("map the first template");
    let shared = load(&engine, &second, second_identity)
        .expect("a template must share the native code its engine already verified and holds");
    let expected = engine.load_template(&wasm).expect("portable template");
    let mut instance = activate(&engine, &shared);
    let mut original = activate(&engine, &expected);
    assert_eq!(
        instance.memory(),
        original.memory(),
        "a template sharing native code must keep its own captured initial memory"
    );
    drop(held);
}

#[test]
fn a_native_payload_is_verified_again_once_no_template_holds_it() {
    let engine = test_engine();
    let directory = published_files::tempdir().expect("directory");
    let (_, [(first, first_identity), (second, second_identity)]) =
        sharing_artifacts(&engine, directory.path());
    tamper(&second.join("template.cwasm"));
    let held = load(&engine, &first, first_identity).expect("map the first template");
    drop(held);
    let refused = load(&engine, &second, second_identity);
    assert!(
        matches!(refused, Err(PreparedError::Invalid(_))),
        "once no template holds the native code, a changed copy must be rejected"
    );
}
