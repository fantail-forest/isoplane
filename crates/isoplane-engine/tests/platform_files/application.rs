use super::{common, filesystem, published_files};

use common::{activate, small_engine_config};
use filesystem::{call, dispatch, ROOT};
use isoplane_abi::filesystem::Operation;
use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{
    Completion, EngineConfig, Instance, InstanceOps, IsoplaneEngine, OccupancyBudget, PlatformFile,
    PlatformFiles, Template,
};

fn files(entries: &[(&str, &[u8])]) -> PlatformFiles {
    PlatformFiles::new(
        entries
            .iter()
            .map(|(path, bytes)| PlatformFile {
                path: (*path).into(),
                bytes: (*bytes).into(),
                modified_ns: 42,
            })
            .collect(),
    )
    .expect("declared immutable files")
}

fn guest(paths: &[&str]) -> Vec<u8> {
    let strings: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (8000 + index as u32 * 128, *path))
        .collect();
    let mut body = String::new();
    for (index, path) in paths.iter().enumerate() {
        let output = 1200 + index as u32 * 200;
        body.push_str(&call(
            Operation::OpenAt,
            &[
                ROOT,
                "(i32.const 0)",
                &format!(
                    "(i32.const {}) (i32.const {})",
                    strings[index].0,
                    path.len()
                ),
                "(i32.const 0)",
                "(i32.const 0)",
                &format!("(i32.const {output})"),
            ],
        ));
        body.push_str(&call(
            Operation::Read,
            &[
                &format!("(i32.load (i32.const {}))", output + 4),
                "(i64.const 128)",
                "(i64.const 0)",
                &format!("(i32.const {})", output + 100),
            ],
        ));
    }
    filesystem::module(&body, &strings)
}

fn result(instance: &mut Instance, index: usize) -> Vec<u8> {
    assert_eq!(
        instance
            .dispatch(&dispatch(), OccupancyBudget::http())
            .completion,
        Completion::Done,
        "reading immutable application files must complete without external I/O"
    );
    let memory = instance.memory();
    let at = 1300 + index * 200;
    assert_eq!(memory[at], 0, "declared application file must be readable");
    let start = u32::from_le_bytes(memory[at + 4..at + 8].try_into().expect("pointer")) as usize;
    let length = u32::from_le_bytes(memory[at + 8..at + 12].try_into().expect("length")) as usize;
    memory[start..start + length].to_vec()
}

fn template(engine: &IsoplaneEngine, wasm: &[u8], content: &[u8]) -> Template {
    engine
        .load_template(wasm)
        .expect("portable template")
        .with_application_files(files(&[("/config.json", content)]))
        .expect("bind application content")
}

#[test]
fn applications_on_one_engine_read_only_their_own_content() {
    let engine = IsoplaneEngine::new(small_engine_config()).expect("engine");
    let wasm = guest(&["config.json"]);
    let first = template(&engine, &wasm, b"first");
    let second = template(&engine, &wasm, b"second");
    assert_ne!(
        first.hash, second.hash,
        "different content must have different snapshot identities"
    );
    assert_eq!(
        result(&mut activate(&engine, &first), 0),
        b"first",
        "first application's file must remain isolated"
    );
    assert_eq!(
        result(&mut activate(&engine, &second), 0),
        b"second",
        "second application's file must remain isolated"
    );
    assert!(
        engine.configuration().platform_files.files().is_empty(),
        "application content must never enter engine-global configuration"
    );
}

#[test]
fn application_content_survives_store_recreation_and_mutable_state_restore() {
    let engine = IsoplaneEngine::new(small_engine_config()).expect("engine");
    let template = template(&engine, &guest(&["config.json"]), b"retained");
    let mut original = activate(&engine, &template);
    assert_eq!(
        result(&mut original, 0),
        b"retained",
        "initial application read"
    );
    let state = original
        .capture_mutable_state()
        .expect("capture explicit state");
    let memory = original.memory().to_vec();
    drop(original);
    let mut restored = activate(&engine, &template);
    restored.memory().copy_from_slice(&memory);
    restored
        .restore_mutable_state(&state)
        .expect("restore explicit state");
    assert_eq!(
        result(&mut restored, 0),
        b"retained",
        "restored instance must retain its application namespace"
    );
    restored.reset().expect("reset Store");
    assert_eq!(
        result(&mut restored, 0),
        b"retained",
        "Store reset must rebuild the same application namespace"
    );
}

#[test]
fn published_application_content_restores_with_the_current_host_trust_file() {
    let wasm = guest(&["config.json", "etc/ssl/certs/ca-certificates.crt"]);
    let engine = IsoplaneEngine::new(EngineConfig {
        platform_files: files(&[("/etc/ssl/certs/ca-certificates.crt", b"old-roots")]),
        ..small_engine_config()
    })
    .expect("producer engine");
    let identity = PreparedIdentity {
        input_hash: *blake3::hash(&wasm).as_bytes(),
        runtime_profile: [2; 32],
        initialization_hash: [3; 32],
    };
    let root = published_files::tempdir().expect("artifact registry");
    let directory = root.path().join("version");
    engine
        .prepare_template_with_files(&wasm, identity, files(&[("/config.json", b"persisted")]))
        .expect("prepare with files")
        .publish(&directory)
        .expect("local publication");
    drop(engine);
    let replacement = IsoplaneEngine::new(EngineConfig {
        platform_files: files(&[("/etc/ssl/certs/ca-certificates.crt", b"new-roots")]),
        ..small_engine_config()
    })
    .expect("replacement engine");
    // SAFETY: unlinking the artifact preserves the contents of retained mappings.
    let template = unsafe { replacement.load_prepared(&directory, identity, &[]) }
        .expect("restore published artifact");
    std::fs::remove_dir_all(&directory).expect("remove backing artifact");
    assert!(
        !directory.exists(),
        "the original publication must no longer resolve"
    );
    let mut instance = activate(&replacement, &template);
    assert_eq!(
        result(&mut instance, 0),
        b"persisted",
        "application content comes from retained local state"
    );
    assert_eq!(
        result(&mut instance, 1),
        b"new-roots",
        "host trust authority must not be frozen in application assets"
    );
}

#[test]
fn application_files_cannot_replace_or_shadow_host_trust_paths() {
    let engine = IsoplaneEngine::new(EngineConfig {
        platform_files: files(&[("/etc/ssl/certs/ca-certificates.crt", b"roots")]),
        ..small_engine_config()
    })
    .expect("engine");
    let wasm = guest(&["config.json"]);
    for path in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc",
        "/etc/ssl/certs/ca-certificates.crt/child",
    ] {
        assert!(
            engine
                .load_template(&wasm)
                .expect("template")
                .with_application_files(files(&[(path, b"override")]))
                .is_err(),
            "asset must not shadow host path {path}"
        );
    }
}

#[test]
fn changed_application_bytes_are_rejected_before_loading() {
    let engine = IsoplaneEngine::new(small_engine_config()).expect("engine");
    let wasm = guest(&["config.json"]);
    let identity = PreparedIdentity {
        input_hash: *blake3::hash(&wasm).as_bytes(),
        runtime_profile: [2; 32],
        initialization_hash: [3; 32],
    };
    let root = published_files::tempdir().expect("registry");
    let path = root.path().join("version");
    let content = b"unique authenticated application content";
    engine
        .prepare_template_with_files(&wasm, identity, files(&[("/config.json", content)]))
        .expect("prepare")
        .publish(&path)
        .expect("publish");
    let state_path = path.join("state.bin");
    let mut bytes = std::fs::read(&state_path).expect("state payload");
    let position = bytes
        .windows(content.len())
        .position(|window| window == content)
        .expect("asset bytes must be in retained state");
    bytes[position] ^= 1;
    std::fs::remove_file(&state_path).expect("remove read-only published state");
    std::fs::write(state_path, bytes).expect("simulate modified backing content");
    assert!(
        // SAFETY: the file changed before loading, which must refuse it.
        unsafe { engine.load_prepared(&path, identity, &[]) }.is_err(),
        "changed content must be refused before an application becomes executable"
    );
}

#[test]
fn binding_files_to_an_active_template_leaves_its_instances_namespace_unchanged() {
    let engine = IsoplaneEngine::new(small_engine_config()).expect("engine");
    let wasm = guest(&["config.json"]);
    let shared = engine.load_template(&wasm).expect("template");
    let mut active = activate(&engine, &shared);
    let bound = shared
        .clone()
        .with_application_files(files(&[("/config.json", b"late")]))
        .expect("a shared template binds files as a new template");
    assert_eq!(
        result(&mut activate(&engine, &bound), 0),
        b"late",
        "the new template must serve the files bound to it"
    );
    active.dispatch(&dispatch(), OccupancyBudget::http());
    assert_ne!(
        active.memory()[1300],
        0,
        "an active template's file namespace cannot change beneath an instance"
    );
}

#[test]
fn a_template_cannot_replace_its_bound_files() {
    let engine = IsoplaneEngine::new(small_engine_config()).expect("engine");
    let bound = template(&engine, &guest(&["config.json"]), b"first");
    assert!(
        bound
            .with_application_files(files(&[("/config.json", b"replacement")]))
            .is_err(),
        "an immutable template cannot silently replace its bound content"
    );
}

#[test]
fn restored_artifacts_cannot_claim_a_path_reserved_by_the_receiving_host() {
    let producer = IsoplaneEngine::new(small_engine_config()).expect("producer");
    let wasm = guest(&["etc/ssl/certs/ca-certificates.crt"]);
    let identity = PreparedIdentity {
        input_hash: *blake3::hash(&wasm).as_bytes(),
        runtime_profile: [2; 32],
        initialization_hash: [3; 32],
    };
    let prepared = producer
        .prepare_template_with_files(
            &wasm,
            identity,
            files(&[(
                "/etc/ssl/certs/ca-certificates.crt",
                b"application claims roots",
            )]),
        )
        .expect("producer with no reserved paths");
    let root = published_files::tempdir().expect("registry");
    let path = root.path().join("version");
    prepared.publish(&path).expect("published artifact");
    let receiver = IsoplaneEngine::new(EngineConfig {
        platform_files: files(&[("/etc/ssl/certs/ca-certificates.crt", b"host roots")]),
        ..small_engine_config()
    })
    .expect("receiving host");
    assert!(
        // SAFETY: nothing changes the published files.
        unsafe { receiver.load_prepared(&path, identity, &[]) }.is_err(),
        "application files cannot replace paths reserved by the receiving host"
    );
}
