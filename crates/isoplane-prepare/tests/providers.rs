mod common;

use isoplane_prepare::{BuildRequest, LocalPreparation, Preparation, RuntimeProfile};
use tokio_util::sync::CancellationToken;

fn file_imports() -> String {
    use isoplane_abi::filesystem::{Operation, IMPORTS};
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
                isoplane_abi::imports::Ty::I32 => "i32",
                isoplane_abi::imports::Ty::I64 => "i64",
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
        (global $files_heap (export "files_heap") (mut i32) (i32.const 4096))
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
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::new(vec![
            isoplane_engine::PlatformFile {
                path: "/config.json".into(),
                bytes: b"application asset".as_slice().into(),
                modified_ns: 42,
            },
        ])
        .expect("application files"),
        engine: engine.configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
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
    let identity = request.identity().expect("input identity");
    // SAFETY: this contract receives output only from the local trusted producer below.
    let prepared = unsafe { engine.decode_builder_output(product.prepared_bytes(), identity) }
        .expect("trusted prepared output");
    let root = tempfile::tempdir().expect("publication");
    let path = root.path().join("prepared");
    prepared.publish(&path).expect("publish");
    // SAFETY: nothing changes the published files while the template maps them.
    let loaded =
        unsafe { engine.load_prepared(&path, identity, &[]) }.expect("load prepared output");
    let memory = loaded.memory_image_arc().to_dense();
    assert_eq!(
        memory[100], b'a',
        "initialization must read immutable application files through the PAL"
    );
    let portable = isoplane_prepare::PreInitializer::new()
        .with_engine_config(request.engine.clone())
        .with_memory_limit(isoplane_abi::config::GuestMemoryBytes::new(4 << 20).unwrap())
        .with_platform_files(request.application_files.clone())
        .run(
            &wasm,
            &isoplane_prepare::PreInitRequest {
                module_hash: isoplane_engine::ModuleHash::of(&wasm),
                init_blob: request.initialization.config.clone(),
            },
        )
        .expect("the SDK's portable preparation path");
    let portable = engine.load_template(&portable.template_wasm).unwrap();
    assert_eq!(
        portable.memory_image_arc().to_dense(),
        memory,
        "the portable SDK path and native preparation preserve the same initialized memory"
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
async fn local_preparation_preserves_the_same_state_as_the_portable_sdk_path() {
    preserves_application_state(&LocalPreparation::new().expect("local provider")).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn layered_preparation_accepts_a_guest_without_an_initializer() {
    let wasm = wat::parse_str(
        r#"(module
        (memory (export "memory") 1 64)
        (data (i32.const 64) "ready")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 2048)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("guest without initializer");
    let engine = common::engine();
    let files = isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
        path: "/config.json".into(),
        bytes: b"asset".as_slice().into(),
        modified_ns: 42,
    }])
    .expect("application asset");
    let request = BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: files.clone(),
        engine: engine.configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        initialization: common::request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let product = LocalPreparation::new()
        .expect("local provider")
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("a guest without an initializer needs no initialized capture boundary");
    assert_eq!(
        product.initialized.passes, 0,
        "no initializer must mean no initialization passes"
    );
    let identity = request.identity().expect("identity");
    // SAFETY: the local trusted preparation provider produced these artifact bytes.
    let prepared = unsafe { engine.decode_builder_output(product.prepared_bytes(), identity) }
        .expect("decode prepared output");
    let template = engine
        .load_prepared_product(&prepared)
        .expect("load prepared guest");
    assert!(
        !template.has_initialize(),
        "ordinary activation must not acquire an initializer"
    );
    assert_eq!(
        &template.memory_image_arc().to_dense()[64..69],
        b"ready",
        "the original guest memory must survive preparation"
    );
    let expected = engine
        .load_template(&wasm)
        .expect("original template")
        .with_application_files(files)
        .expect("bind application asset");
    assert_eq!(
        template.hash, expected.hash,
        "the artifact must retain application files in its identity"
    );
}
