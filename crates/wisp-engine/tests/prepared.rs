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

fn compiled_artifact() -> (wisp_engine::WispEngine, tempfile::TempDir, PreparedIdentity) {
    let engine = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let expected = identity(&wasm);
    let directory = tempfile::tempdir().expect("compiled artifact");
    engine
        .prepare_template(&wasm, expected)
        .expect("prepare")
        .publish(
            &directory.path().join("version"),
            PreparedStorage::VerifiedCopy,
        )
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
    let restored = unsafe {
        // SAFETY: this test compiled the artifact into its own private fixture storage.
        engine.restore_prepared(&path, digest, expected.runtime_profile)
    }
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
            unsafe {
                // SAFETY: this test compiled the artifact into its own private fixture storage.
                engine.restore_prepared(
                    &directory.path().join("version"),
                    [0; 32],
                    expected.runtime_profile,
                )
            },
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
            unsafe {
                // SAFETY: this test compiled the artifact into its own private fixture storage.
                engine.restore_prepared(&path, digest, [0; 32])
            },
            Err(PreparedError::Incompatible("catalog runtime profile"))
        ),
        "the selected runtime must match the compiled prepared state"
    );
}

#[test]
fn builder_output_cannot_replace_host_platform_files() {
    let producer = test_engine();
    let wasm = guest(4, "", "", DONE, DONE);
    let declared = wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
        path: "/etc/ssl/certs/ca-certificates.crt".into(),
        bytes: b"roots".as_slice().into(),
        modified_ns: 42,
    }])
    .expect("declared files");
    let prepared = producer
        .prepare_template_with_files(&wasm, identity(&wasm), declared.clone())
        .expect("producer without reserved paths");
    let receiver = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
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
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("prepared");
    engine
        .prepare_template(&wasm, identity)
        .expect("prepare")
        .publish(&path, PreparedStorage::VerifiedCopy)
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
        unsafe {
            // SAFETY: this test compiled the artifact into its own private fixture storage.
            engine.restore_prepared(&path, digest, identity.runtime_profile)
        }
        .is_err(),
        "a manifest receipt alone must not qualify missing executable payloads"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn profiling_an_existing_template_emits_native_symbols_without_rebuilding_it() {
    const CHILD: &str = "WISP_PREPARED_PROFILE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().expect("owned profiler output directory");
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "profiling_an_existing_template_emits_native_symbols_without_rebuilding_it",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .current_dir(directory.path())
            .output()
            .expect("isolated profiler test");
        assert!(
            output.status.success(),
            "profiling must work in an isolated process: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
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
    let directory = tempfile::tempdir().expect("prepared artifact");
    let path = directory.path().join("version");
    engine
        .prepare_template(&wasm, expected)
        .expect("prepare without profiling")
        .publish(&path, PreparedStorage::VerifiedCopy)
        .expect("publish");
    let dump = std::path::PathBuf::from(format!("jit-{}.dump", std::process::id()));
    assert!(
        !dump.exists(),
        "normal engine operation must not enable profiling output"
    );
    let profiled = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
        perf_jitdump: true,
        ..small_engine_config()
    })
    .expect("profiled engine");
    let template = unsafe {
        // SAFETY: this test compiled the artifact into its own private fixture storage.
        profiled.load_prepared(&path, expected)
    }
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
    let config = wisp_engine::EngineConfig {
        perf_jitdump: true,
        ..small_engine_config()
    };
    let bytes = postcard::to_allocvec(&config).expect("serialize preparation settings");
    let restored: wisp_engine::EngineConfig =
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
