mod common;
use common::{engine, guest, request};
use isoplane_abi::config::InitSandbox;
use isoplane_prepare::{initialize, PrepareError};

#[test]
fn initialization_failure_preserves_the_diagnosis_before_a_generic_final_log() {
    let wasm = guest(
        r#"(import "isoplane" "log_emit" (func $log (param i32 i32 i32)))
        (data (i32.const 64) "missing application setting")
        (data (i32.const 128) "initialization failed")"#,
        r#"(call $log (i32.const 3) (i32.const 64) (i32.const 27))
        (call $log (i32.const 3) (i32.const 128) (i32.const 21)) unreachable"#,
    );
    let error = initialize(&engine(), &wasm, &request())
        .err()
        .expect("the initializer traps");
    let PrepareError::Initialization { detail, .. } = error else {
        panic!("a logged guest trap must remain an initialization failure: {error}");
    };
    assert!(
        detail.contains("missing application setting") && detail.contains("initialization failed"),
        "the generic final log must not erase the guest's earlier diagnosis: {detail}"
    );
}

#[test]
fn baking_preserves_grown_memory_hidden_globals_and_static_tables() {
    let wasm = guest("", "(drop (memory.grow (i32.const 1))) (i32.store (i32.const 70000) (i32.const 123)) (global.set $state (i32.const 42))");
    let prepared = initialize(&engine(), &wasm, &request())
        .expect("complete initialized state must round-trip through the bake oracle");
    assert_eq!(
        prepared.initialized.passes, 1,
        "pure initialization must finish in one pass"
    );
    assert!(
        prepared.initialized.deferred.is_empty(),
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
        r#"(import "isoplane" "clock_now_coarse" (func $clock (result i64)))"#,
        "(drop (call $clock))",
    );
    let error = initialize(&engine(), &wasm, &request())
        .err()
        .expect("sealed clock access must fail");
    assert!(
        matches!(error, PrepareError::Unattributable { ref import, .. } if import == "clock_now_coarse"),
        "forbidden capabilities must terminate initialization"
    );
    let failure = isoplane_prepare::WorkerFailure::new(
        isoplane_prepare::BuildStage::Initialization,
        error.into(),
    );
    assert_eq!(
        failure.completion,
        Some(isoplane_engine::Completion::Trapped),
        "capability refusals must retain the guest completion in the isolated worker report"
    );
}

#[test]
fn runtime_bringup_answers_deterministic_clock_without_widening_unit_policy() {
    let wasm = guest(
        r#"(import "isoplane" "clock_now_coarse" (func $clock (result i64)))"#,
        "(i64.store (i32.const 64) (call $clock))",
    );
    let mut request = request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    let result = initialize(&engine(), &wasm, &request).expect("runtime clock admission");
    assert_eq!(
        result.initialized.pal_calls,
        vec![("clock_now_coarse".to_string(), 1)],
        "the report must count the admitted runtime clock exactly once"
    );
}

#[test]
fn an_infinite_initializer_is_killed_by_the_shared_engine_budget() {
    let wasm = guest("", "(loop $forever (br $forever))");
    let mut request = request();
    request.occupancy_ms = 1;
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
        r#"(import "isoplane" "init_unit_begin" (func $begin (param i32)))
        (import "isoplane" "init_unit_deferred" (func $deferred (param i32) (result i32)))
        (import "isoplane" "clock_now_coarse" (func $clock (result i64)))"#,
        r#"(call $begin (i32.const 7))
        (if (i32.eqz (call $deferred (i32.const 7)))
          (then (i32.store (i32.const 64) (i32.const 99)) (drop (call $clock))))"#,
    );
    let mut request = request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    let result = initialize(&engine(), &wasm, &request)
        .expect("a forbidden unit must defer with a pristine replay");
    assert_eq!(
        result.initialized.deferred,
        vec![7],
        "the forbidden unit must be recorded explicitly"
    );
    assert_eq!(
        result.initialized.capability_refusals, 1,
        "the report must retain the rejected pass's refusal"
    );
    assert_eq!(
        result.initialized.deferred_reasons,
        vec![(7, "clock_now_coarse".to_string())],
        "the report must identify why the unit was deferred"
    );
    assert_eq!(
        result.initialized.passes, 2,
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
        r#"(import "isoplane" "init_hash_seed" (func $seed (param i32 i32)))"#,
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
fn preparation_accepts_a_timer_interval_of_a_second_and_refuses_a_shorter_one() {
    let opting_in = |minimum_interval_ms: u64| {
        guest(
            r#"(import "isoplane" "timers_between_requests" (func $timers (param i64) (result i64)))"#,
            &format!("(drop (call $timers (i64.const {minimum_interval_ms})))"),
        )
    };
    assert!(
        matches!(
            initialize(&engine(), &opting_in(999), &request()),
            Err(PrepareError::Initialization { .. })
        ),
        "an interval under a second must fail preparation"
    );
    initialize(&engine(), &opting_in(1_000), &request())
        .expect("an interval of a second must be accepted");
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
    use isoplane_engine::bindings::{BindingError, InitializationBindings};
    use isoplane_engine::{Completion, ImportCall, OccupancyBudget};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
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
        r#"(import "isoplane" "http_body_read" (func $read (param i32 i32) (result i64)))"#,
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
    let result = instance.run_initialize(OccupancyBudget {
        occupancy_ms: 1000,
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
        r#"(import "isoplane" "init_unit_begin" (func $begin (param i32)))
        (import "isoplane" "init_unit_deferred" (func $deferred (param i32) (result i32)))
        (import "isoplane" "clock_now_coarse" (func $clock (result i64)))"#,
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
        isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
            path: "/etc/ssl/certs/ca-certificates.crt".into(),
            bytes: std::sync::Arc::from(&b"declared root material"[..]),
            modified_ns: 123,
        }])
        .expect("declared platform files");
    let encoded = serde_json::to_vec(&configuration).expect("encode worker configuration");
    let restored: isoplane_engine::EngineConfig =
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
        serde_json::from_value::<isoplane_engine::EngineConfig>(encoded).is_err(),
        "deserializing worker configuration must enforce the platform filesystem boundary"
    );
}

#[test]
fn initialization_identity_changes_with_the_declared_trust_snapshot() {
    let wasm = guest("", "");
    let mut request = isoplane_prepare::BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: engine().configuration(),
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        limits: isoplane_abi::config::Limits::default(),
        initialization: request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
        portable_template: isoplane_prepare::PortableTemplate::Returned,
    };
    let old = request.identity().expect("initial identity");
    request.engine.platform_files =
        isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
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
    let mut request = isoplane_prepare::BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: engine().configuration(),
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        limits: isoplane_abi::config::Limits::default(),
        initialization: request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
        portable_template: isoplane_prepare::PortableTemplate::Returned,
    };
    let before = request.identity().expect("original identity");
    request.application_files =
        isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
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
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (table.set (i32.const 0) (ref.null func)) (i64.const 0))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("valid runtime table mutation");
    let result = initialize(&engine(), &wasm, &request())
        .expect("a module without initialization must retain supported runtime capabilities");
    assert_eq!(
        result.wasm, wasm,
        "no initializer means no portable module rewrite"
    );
    assert_eq!(
        result.initialized.passes, 0,
        "a module without an initializer cannot execute preparation passes"
    );
}

#[test]
fn preparation_captures_its_settings_and_changes_identity_when_they_change() {
    let wasm = guest(
        r#"(import "isoplane" "runtime_config_get" (func $settings (param i32 i32) (result i32)))"#,
        "(drop (call $settings (i32.const 128) (i32.const 64)))",
    );
    let mut inputs = request();
    let mut identities = Vec::new();
    for settings in [
        br#"{"token":"first"}"#.as_slice(),
        br#"{"token":"second"}"#.as_slice(),
        b"{}".as_slice(),
    ] {
        inputs.runtime_config = settings.to_vec();
        identities.push(
            inputs
                .digest(*blake3::hash(&wasm).as_bytes())
                .expect("settings identity"),
        );
        let output = initialize(&engine(), &wasm, &inputs)
            .expect("settings must be readable during preparation");
        let template = engine()
            .load_template(&output.wasm)
            .expect("captured settings template");
        assert_eq!(
            &template.memory_image_arc().to_dense()[128..128 + settings.len()],
            settings,
            "each template must retain its own configuration revision"
        );
    }
    assert!(
        identities.windows(2).all(|pair| pair[0] != pair[1]),
        "replacement and deletion must invalidate captured state even with unchanged code"
    );
}

#[test]
fn invalid_preparation_settings_are_refused_without_exposing_values() {
    let wasm = guest("", "");
    for settings in [
        b"private-value".to_vec(),
        vec![b' '; isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES + 1],
    ] {
        let mut inputs = request();
        inputs.runtime_config = settings;
        assert!(
            matches!(
                initialize(&engine(), &wasm, &inputs),
                Err(PrepareError::InvalidRequest(_))
            ),
            "invalid or oversized settings must fail before guest execution"
        );
    }
}
