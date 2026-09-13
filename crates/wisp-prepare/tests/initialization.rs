mod common;
use common::{engine, guest, request};
use wisp_abi::config::InitSandbox;
use wisp_prepare::{initialize, PrepareError};

#[test]
fn baking_preserves_grown_memory_hidden_globals_and_static_tables() {
    let wasm = guest("", "(drop (memory.grow (i32.const 1))) (i32.store (i32.const 70000) (i32.const 123)) (global.set $state (i32.const 42))");
    let prepared = initialize(&engine(), &wasm, &request())
        .expect("complete initialized state must round-trip through the bake oracle");
    assert_eq!(
        prepared.passes, 1,
        "pure initialization must finish in one pass"
    );
    assert!(
        prepared.deferred.is_empty(),
        "pure initialization must not defer units"
    );
    let template = engine()
        .load_template(&prepared.wasm)
        .expect("baked WASM must be loadable");
    assert_eq!(
        template.memory_pages(),
        2,
        "baked memory must include initialization growth"
    );
    assert!(
        !template.has_initialize(),
        "activation must not be able to repeat initialization"
    );
    assert_eq!(
        &template.memory_image_arc().to_dense()[70000..70004],
        &123_i32.to_le_bytes(),
        "baked memory must preserve initialization writes"
    );
}

#[test]
fn a_sealed_initializer_clock_attempt_fails_before_a_host_effect() {
    let wasm = guest(
        r#"(import "wisp" "clock_now_coarse" (func $clock (result i64)))"#,
        "(drop (call $clock))",
    );
    let error = initialize(&engine(), &wasm, &request())
        .err()
        .expect("sealed clock access must fail");
    assert!(
        matches!(error, PrepareError::Unattributable { ref import, .. } if import == "clock_now_coarse"),
        "forbidden capabilities must terminate initialization"
    );
    let failure =
        wisp_prepare::WorkerFailure::new(wisp_prepare::BuildStage::Initialization, error.into());
    assert_eq!(
        failure.completion,
        Some(wisp_engine::Completion::Trapped),
        "capability refusals must retain the guest completion in the isolated worker report"
    );
}

#[test]
fn runtime_bringup_answers_deterministic_clock_without_widening_unit_policy() {
    let wasm = guest(
        r#"(import "wisp" "clock_now_coarse" (func $clock (result i64)))"#,
        "(i64.store (i32.const 64) (call $clock))",
    );
    let mut request = request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    let result = initialize(&engine(), &wasm, &request).expect("runtime clock admission");
    assert_eq!(
        result.pal_calls,
        vec![("clock_now_coarse".to_string(), 1)],
        "the report must count the admitted runtime clock exactly once"
    );
}

#[test]
fn an_infinite_initializer_is_killed_by_the_shared_engine_budget() {
    let wasm = guest("", "(loop $forever (br $forever))");
    let mut request = request();
    request.cpu_ms = 1;
    let error = initialize(&engine(), &wasm, &request)
        .err()
        .expect("unbounded initialization must fail");
    assert!(
        matches!(error, PrepareError::Initialization { .. }),
        "engine budget failure must end the initialization job"
    );
}

#[test]
fn a_table_mutation_is_refused_before_preparation_can_lose_it() {
    let wasm = guest("", "(table.set (i32.const 0) (ref.null func))");
    assert!(
        matches!(
            initialize(&engine(), &wasm, &request()),
            Err(PrepareError::State(_))
        ),
        "mutable table state must not be silently omitted from a template"
    );
}

#[test]
fn a_forbidden_unit_replays_from_pristine_state_and_is_deferred() {
    let wasm = guest(
        r#"(import "wisp" "init_unit_begin" (func $begin (param i32)))
        (import "wisp" "init_unit_deferred" (func $deferred (param i32) (result i32)))
        (import "wisp" "clock_now_coarse" (func $clock (result i64)))"#,
        r#"(call $begin (i32.const 7))
        (if (i32.eqz (call $deferred (i32.const 7)))
          (then (i32.store (i32.const 64) (i32.const 99)) (drop (call $clock))))"#,
    );
    let mut request = request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    let result = initialize(&engine(), &wasm, &request)
        .expect("a forbidden unit must defer with a pristine replay");
    assert_eq!(
        result.deferred,
        vec![7],
        "the forbidden unit must be recorded explicitly"
    );
    assert_eq!(
        result.capability_refusals, 1,
        "the report must retain the rejected pass's refusal"
    );
    assert_eq!(
        result.deferred_reasons,
        vec![(7, "clock_now_coarse".to_string())],
        "the report must identify why the unit was deferred"
    );
    assert_eq!(
        result.passes, 2,
        "deferring a unit must replay initialization"
    );
    let template = engine()
        .load_template(&result.wasm)
        .expect("the replayed template must load");
    assert_eq!(
        &template.memory_image_arc().to_dense()[64..68],
        &[0; 4],
        "writes preceding the forbidden call must not leak into the template"
    );
}

#[test]
fn an_invalid_seed_buffer_traps_instead_of_baking_zero_entropy() {
    let wasm = guest(
        r#"(import "wisp" "init_hash_seed" (func $seed (param i32 i32)))"#,
        "(call $seed (i32.const 64) (i32.const 15))",
    );
    assert!(
        matches!(
            initialize(&engine(), &wasm, &request()),
            Err(PrepareError::Initialization { .. })
        ),
        "invalid seed buffers must fail even though the import returns no value"
    );
}

#[test]
fn wasm_start_code_cannot_execute_before_the_initializer_budget_is_armed() {
    let wasm = wat::parse_str("(module (memory (export \"memory\") 1) (func $start (loop $forever (br $forever))) (start $start))").expect("the start fixture must assemble");
    assert!(
        matches!(
            initialize(&engine(), &wasm, &request()),
            Err(PrepareError::State(_))
        ),
        "unbudgeted wasm start functions must be refused before instantiation"
    );
}

#[test]
fn a_future_import_traps_before_calling_an_initialization_provider() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use wisp_engine::bindings::{BindingError, InitializationBindings};
    use wisp_engine::{Completion, CpuBudget, ImportCall};
    struct Provider(Arc<AtomicUsize>);
    impl InitializationBindings for Provider {
        fn call_sync(&mut self, call: ImportCall<'_>) -> Result<i64, BindingError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(BindingError::Unsupported(call.import))
        }
        fn call_fill(
            &mut self,
            call: ImportCall<'_>,
            _out: &mut [u8],
        ) -> Result<i64, BindingError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(BindingError::Unsupported(call.import))
        }
        fn runtime_bringup(&self) -> bool {
            false
        }
    }
    let wasm = guest(
        r#"(import "wisp" "http_body_read" (func $read (param i32 i32) (result i64)))"#,
        "(drop (call $read (i32.const 1) (i32.const 32)))",
    );
    let engine = engine();
    let template = engine
        .load_template(&wasm)
        .expect("the declared future import must link");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut instance = engine
        .activate_initialization(&template, Box::new(Provider(calls.clone())))
        .expect("restricted activation must succeed");
    let result = instance.run_initialize(CpuBudget {
        cpu_ms: 1000,
        wall_ms_cap: 5000,
        spent_ns: 0,
    });
    assert_eq!(
        result.completion,
        Completion::Trapped,
        "future imports must trap during initialization"
    );
    assert_eq!(
        instance.last_capability_violation(),
        Some("http_body_read"),
        "the denial must name the requested host operation"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "the capability gate must fire before invoking the initialization provider"
    );
}

#[test]
fn ignoring_a_deferred_unit_does_not_reenter_runtime_bringup() {
    let wasm = guest(
        r#"(import "wisp" "init_unit_begin" (func $begin (param i32)))
        (import "wisp" "init_unit_deferred" (func $deferred (param i32) (result i32)))
        (import "wisp" "clock_now_coarse" (func $clock (result i64)))"#,
        r#"(call $begin (i32.const 7))
        (drop (call $deferred (i32.const 7)))
        (drop (call $clock))"#,
    );
    let mut request = request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    assert!(
        matches!(
            initialize(&engine(), &wasm, &request),
            Err(PrepareError::DeferralIgnored { unit: 7, .. })
        ),
        "a unit that ignores deferral must fail instead of acquiring runtime-only capabilities"
    );
}

#[test]
fn preparation_transport_preserves_only_declared_platform_files() {
    let mut configuration = engine().configuration();
    configuration.platform_files =
        wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
            path: "/etc/ssl/certs/ca-certificates.crt".into(),
            bytes: std::sync::Arc::from(&b"declared root material"[..]),
            modified_ns: 123,
        }])
        .expect("declared platform files");
    let encoded = serde_json::to_vec(&configuration).expect("encode worker configuration");
    let restored: wisp_engine::EngineConfig =
        serde_json::from_slice(&encoded).expect("decode worker configuration");
    let files = restored.platform_files.files();
    assert_eq!(
        files.len(),
        1,
        "isolated preparation must receive exactly the declared files"
    );
    assert_eq!(
        files[0].path, "/etc/ssl/certs/ca-certificates.crt",
        "the declared trust path must survive process transport"
    );
    assert_eq!(
        &*files[0].bytes, b"declared root material",
        "preparation must observe the same trust bytes as execution"
    );
    assert_eq!(
        files[0].modified_ns, 123,
        "the trust recheck timestamp must survive transport"
    );
}

#[test]
fn preparation_transport_rejects_unvalidated_platform_file_paths() {
    let configuration = engine().configuration();
    let mut encoded = serde_json::to_value(configuration).expect("worker configuration");
    encoded["platform_files"] =
        serde_json::json!([{"path":"/etc/../secret", "bytes":[], "modified_ns":0}]);
    assert!(
        serde_json::from_value::<wisp_engine::EngineConfig>(encoded).is_err(),
        "deserializing worker configuration must enforce the platform filesystem boundary"
    );
}

#[test]
fn initialization_identity_changes_with_the_declared_trust_snapshot() {
    let wasm = guest("", "");
    let mut request = wisp_prepare::BuildRequest {
        guest_memory_bytes: None,
        application_files: wisp_engine::PlatformFiles::default(),
        engine: engine().configuration(),
        runtime: wisp_prepare::RuntimeProfile::RawWasm,
        limits: wisp_abi::config::Limits::default(),
        initialization: request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let old = request.identity().expect("initial identity");
    request.engine.platform_files =
        wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
            path: "/etc/ssl/certs/ca-certificates.crt".into(),
            bytes: std::sync::Arc::from(&b"new roots"[..]),
            modified_ns: 1,
        }])
        .expect("new declared trust material");
    let new = request.identity().expect("updated identity");
    assert_ne!(
        old.initialization_hash, new.initialization_hash,
        "trust-visible preparation changes must change the prepared input identity"
    );
    assert_eq!(
        old.runtime_profile, new.runtime_profile,
        "trust rotation must not change the runtime compatibility profile"
    );
}

#[test]
fn initialization_identity_binds_application_content_independently_of_the_runtime() {
    let wasm = guest("", "");
    let mut request = wisp_prepare::BuildRequest {
        guest_memory_bytes: None,
        application_files: wisp_engine::PlatformFiles::default(),
        engine: engine().configuration(),
        runtime: wisp_prepare::RuntimeProfile::RawWasm,
        limits: wisp_abi::config::Limits::default(),
        initialization: request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let before = request.identity().expect("original identity");
    request.application_files = wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
        path: "/config.json".into(),
        bytes: b"content".as_slice().into(),
        modified_ns: 42,
    }])
    .expect("application files");
    let after = request.identity().expect("content identity");
    assert_ne!(
        before.initialization_hash, after.initialization_hash,
        "application file changes must invalidate prepared initialization state"
    );
    assert_eq!(
        before.runtime_profile, after.runtime_profile,
        "application content must not alter runtime provenance"
    );
}

#[test]
fn a_module_without_initialization_retains_its_runtime_table_operations() {
    let wasm = wat::parse_str(
        r#"(module
        (memory (export "memory") 1 1)
        (table (export "table") 1 funcref)
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "wisp_handle") (param i32 i32) (result i64)
            (table.set (i32.const 0) (ref.null func)) (i64.const 0))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("valid runtime table mutation");
    let result = initialize(&engine(), &wasm, &request())
        .expect("a module without initialization must retain supported runtime capabilities");
    assert_eq!(
        result.wasm, wasm,
        "no initializer means no portable module rewrite"
    );
    assert_eq!(
        result.passes, 0,
        "a module without an initializer cannot execute preparation passes"
    );
}
