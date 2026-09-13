mod common;

use tokio_util::sync::CancellationToken;
use wisp_prepare::{BuildRequest, LocalPreparation, Preparation, RuntimeProfile};

fn file_imports() -> String {
    use wisp_abi::filesystem::{Operation, IMPORTS};
    let mut imports = String::new();
    for (operation, name) in [
        (Operation::Directories, "directories"),
        (Operation::OpenAt, "open"),
        (Operation::Read, "read"),
    ] {
        let definition = IMPORTS
            .iter()
            .find(|definition| definition.operation == operation)
            .expect("canonical filesystem operation");
        let signature = definition.pal.sig.expect("filesystem signature");
        let parameters = signature
            .params
            .iter()
            .map(|ty| match ty {
                wisp_abi::imports::Ty::I32 => "i32",
                wisp_abi::imports::Ty::I64 => "i64",
            })
            .collect::<Vec<_>>()
            .join(" ");
        imports.push_str(&format!(
            "(import \"{}\" \"{}\" (func ${name} (param {parameters})))",
            definition.pal.module, definition.pal.func
        ));
    }
    imports.push_str(
        r#"(data (i32.const 8000) "config.json")
        (global $files_heap (mut i32) (i32.const 4096))
        (func (export "cabi_realloc") (param i32 i32 i32) (param $size i32) (result i32)
            (local $old i32)
            (local.set $old (global.get $files_heap))
            (global.set $files_heap (i32.and (i32.add (i32.add (local.get $old) (local.get $size)) (i32.const 15)) (i32.const -16)))
            (local.get $old))"#,
    );
    imports
}

fn read_file() -> &'static str {
    r#"(call $directories (i32.const 1000))
       (call $open (i32.load (i32.load (i32.const 1000))) (i32.const 0) (i32.const 8000) (i32.const 11) (i32.const 0) (i32.const 0) (i32.const 1200))
       (call $read (i32.load (i32.const 1204)) (i64.const 128) (i64.const 0) (i32.const 1300))
       (if (i32.load8_u (i32.const 1300)) (then unreachable))
       (i32.store8 (i32.const 100) (i32.load8_u (i32.load (i32.const 1304))))"#
}

async fn preserves_application_state(provider: &dyn Preparation) {
    let wasm = common::guest(
        &file_imports(),
        &format!("(global.set $state (i32.const 42)) {}", read_file()),
    );
    let engine = common::engine();
    let request = BuildRequest {
        application_files: wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
            path: "/config.json".into(),
            bytes: b"application asset".as_slice().into(),
            modified_ns: 42,
        }])
        .expect("application files"),
        engine: engine.configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        limits: wisp_abi::config::Limits::default(),
        initialization: common::request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let product = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("the selected provider must satisfy the shared preparation contract");
    assert_eq!(
        product.initialized.seed_reads, 0,
        "this representation comparison uses no random inputs"
    );
    assert_eq!(
        product.initialized.passes, 1,
        "the provider must run one successful initialization"
    );
    let template = engine
        .load_template(&product.initialized.wasm)
        .expect("initialized portable result");
    assert_eq!(
        template.memory_image_arc().to_dense()[100],
        b'a',
        "initialization must read immutable application files through the PAL"
    );
    let expected_hash = template
        .with_application_files(request.application_files.clone())
        .expect("bind expected files")
        .hash;
    let identity = request.identity().expect("input identity");
    // SAFETY: the local trusted provider below produced these bytes.
    let prepared = unsafe { engine.decode_builder_output(product.prepared_bytes(), identity) }
        .expect("trusted prepared output");
    let root = tempfile::tempdir().expect("publication");
    let path = root.path().join("prepared");
    prepared
        .publish(&path, wisp_engine::prepared::PreparedStorage::VerifiedCopy)
        .expect("publish");
    let loaded = engine
        .load_prepared(&path, identity)
        .expect("load prepared output");
    assert_eq!(
        loaded.hash, expected_hash,
        "prepared identity must include the exact application content"
    );
    assert!(
        !loaded.has_initialize(),
        "the provider must remove initialization from its output"
    );
    assert!(
        !product.prepared_bytes().is_empty(),
        "the provider must return native and explicit-state transport"
    );
}

#[tokio::test]
async fn local_preparation_satisfies_the_shared_provider_contract() {
    preserves_application_state(&LocalPreparation::new().expect("portable provider")).await;
}
