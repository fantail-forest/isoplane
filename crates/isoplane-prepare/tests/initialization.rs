mod common;
use common::{engine, engine_config, request};
use isoplane_harness::preparation::{self, staged_guest, staged_guest_with_handle, Prepared};
use isoplane_prepare::{Initialization, PrepareError};
use std::collections::BTreeMap;

/// Prepares `wasm` the way a node prepares an application on a shared runtime.
fn prepare(wasm: &[u8], initialization: Initialization) -> Result<Prepared, PrepareError> {
    preparation::prepare(&preparation::request(wasm, engine_config(), initialization))
}

/// What preparing `wasm`'s runtime stage as a baseline refused. A module
/// whose state preparation cannot capture is refused there.
fn baseline_refusal(wasm: &[u8]) -> PrepareError {
    preparation::baseline_refusal(&preparation::request(wasm, engine_config(), request()))
}

/// What preparing `wasm`'s application stage refused, its runtime stage
/// having prepared as a baseline.
fn refusal(wasm: &[u8], initialization: Initialization) -> PrepareError {
    preparation::application_refusal(&preparation::request(wasm, engine_config(), initialization))
}

#[test]
fn initialization_receives_fresh_entropy() {
    let wasm = staged_guest(
        r#"(import "isoplane" "entropy_fill" (func $entropy (param i32 i32)))"#,
        "",
        "(call $entropy (i32.const 96) (i32.const 32))",
    );
    let first = prepare(&wasm, request()).unwrap().memory();
    let second = prepare(&wasm, request()).unwrap().memory();
    assert_ne!(&first[96..128], &second[96..128]);
}

#[test]
fn initialization_failure_preserves_the_diagnosis_before_a_generic_final_log() {
    let wasm = staged_guest(
        r#"(import "isoplane" "log_emit" (func $log (param i32 i32 i32)))
        (data (i32.const 64) "missing application setting")
        (data (i32.const 128) "initialization failed")"#,
        "",
        r#"(call $log (i32.const 3) (i32.const 64) (i32.const 27))
        (call $log (i32.const 3) (i32.const 128) (i32.const 21)) unreachable"#,
    );
    let error = refusal(&wasm, request());
    let PrepareError::Initialization {
        completion: isoplane_engine::Completion::Trapped,
        detail,
    } = error
    else {
        panic!("a logged guest trap must remain an initialization failure: {error}");
    };
    assert!(
        detail.contains("missing application setting") && detail.contains("initialization failed"),
        "the generic final log must not erase the guest's earlier diagnosis: {detail}"
    );
}

#[test]
fn a_template_carries_the_memory_its_initializer_grew_and_the_globals_it_set() {
    let wasm = staged_guest_with_handle(
        "",
        "",
        "(drop (memory.grow (i32.const 1))) (i32.store (i32.const 140000) (i32.const 123)) (global.set $state (i32.const 42))",
        "(i32.store (i32.const 512) (global.get $runtime_booted)) (i32.store (i32.const 516) (global.get $state)) (i64.const 0)",
    );
    let prepared = prepare(&wasm, request()).expect("grown memory and set globals are capturable");
    assert_eq!(
        prepared.template.memory_pages(),
        3,
        "the template's memory must include initialization growth"
    );
    assert!(
        !prepared.template.has_initialize(),
        "activation must not be able to repeat initialization"
    );
    assert_eq!(
        &prepared.memory()[140000..140004],
        &123_i32.to_le_bytes(),
        "the template's memory must preserve initialization writes to the grown page"
    );
    assert_eq!(
        &prepared.memory_after_dispatch()[512..520],
        [1_i32.to_le_bytes(), 42_i32.to_le_bytes()].as_flattened(),
        "an instance must read `runtime_booted` and `state` as initialization left them"
    );
}

#[test]
fn an_application_on_a_shared_runtime_is_held_to_its_memory_limit_during_initialization() {
    let wasm = staged_guest(
        "",
        "",
        "(if (i32.ne (memory.grow (i32.const 2)) (i32.const -1)) (then unreachable))
        (if (i32.ne (memory.grow (i32.const 1)) (i32.const 2)) (then unreachable))",
    );
    let three_pages = 3 * isoplane_abi::WASM_PAGE_SIZE as u64;
    let request = isoplane_prepare::BuildRequest {
        guest_memory_bytes: Some(
            isoplane_abi::config::GuestMemoryBytes::new(three_pages).expect("a memory limit"),
        ),
        ..preparation::request(&wasm, engine_config(), request())
    };
    let prepared = preparation::prepare(&request).expect(
        "a runtime may declare more memory than its application's limit, and growth past that limit must be refused without a trap",
    );
    assert_eq!(
        prepared.template.memory_pages(),
        3,
        "initialization must be able to grow to its application's limit"
    );
}

#[test]
fn a_forbidden_import_ends_initialization_with_the_guests_trap_and_is_named() {
    let wasm = staged_guest(
        r#"(import "isoplane" "future_cancel" (func $cancel (param i64)))"#,
        "",
        "(call $cancel (i64.const 1))",
    );
    let error = refusal(&wasm, request());
    assert!(
        matches!(
            error,
            PrepareError::ForbiddenCapability {
                ref import,
                completion: isoplane_engine::Completion::Trapped,
                ..
            } if import == "future_cancel"
        ),
        "a forbidden capability must end initialization with the guest's trap: {error}"
    );
}

#[test]
fn initialization_answers_and_counts_the_clock() {
    let wasm = staged_guest(
        r#"(import "isoplane" "clock_now_coarse" (func $clock (result i64)))"#,
        "",
        "(i64.store (i32.const 64) (call $clock))",
    );
    let result = prepare(&wasm, request()).expect("clock admission");
    assert_eq!(
        result.initialized.pal_calls,
        vec![isoplane_engine::PalCallCount {
            import: "clock_now_coarse".into(),
            calls: 1,
        }],
        "the report must count the admitted clock exactly once"
    );
}

#[test]
fn an_infinite_initializer_is_killed_by_the_shared_engine_budget() {
    let wasm = staged_guest("", "", "(loop $forever (br $forever))");
    let mut request = request();
    request.occupancy_ms = 1;
    let error = refusal(&wasm, request);
    assert!(
        matches!(
            error,
            PrepareError::Initialization {
                completion: isoplane_engine::Completion::BudgetKill,
                ..
            }
        ),
        "an initializer still running past its budget must be killed, not left to trap or finish: {error}"
    );
}

#[test]
fn a_table_mutation_is_refused_before_preparation_can_lose_it() {
    let wasm = staged_guest(
        r#"(table (export "table") 1 funcref)"#,
        "",
        "(table.set (i32.const 0) (ref.null func))",
    );
    let error = baseline_refusal(&wasm);
    assert!(
        matches!(error, PrepareError::State(_)),
        "mutable table state must not be silently omitted from a template: {error}"
    );
}

#[test]
fn wasm_start_code_cannot_execute_before_the_initializer_budget_is_armed() {
    let wasm = staged_guest(
        "(func $start (loop $forever (br $forever))) (start $start)",
        "",
        "",
    );
    let error = baseline_refusal(&wasm);
    assert!(
        matches!(error, PrepareError::State(_)),
        "unbudgeted wasm start functions must be refused before instantiation: {error}"
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
        fn begin_slice(&mut self) {}
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
    }
    let wasm = staged_guest(
        r#"(import "isoplane" "http_body_read" (func $read (param i32 i32) (result i64)))"#,
        "",
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
        slice_wall_ms_cap: 5000,
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
fn engine_configuration_preserves_only_declared_platform_files() {
    let mut configuration = engine_config();
    configuration.platform_files =
        isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
            path: "/etc/ssl/certs/ca-certificates.crt".into(),
            bytes: std::sync::Arc::from(&b"declared root material"[..]),
            modified_ns: 123,
        }])
        .expect("declared platform files");
    let encoded = serde_json::to_vec(&configuration).expect("encode engine configuration");
    let restored: isoplane_engine::EngineConfig =
        serde_json::from_slice(&encoded).expect("decode engine configuration");
    let files = restored.platform_files.files();
    assert_eq!(
        files.len(),
        1,
        "the decoded engine configuration must contain exactly the declared files"
    );
    assert_eq!(
        files[0].path, "/etc/ssl/certs/ca-certificates.crt",
        "the declared trust path must survive encoding"
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
fn engine_configuration_rejects_unvalidated_platform_file_paths() {
    let configuration = engine_config();
    let mut encoded = serde_json::to_value(configuration).expect("engine configuration");
    encoded["platform_files"] =
        serde_json::json!([{"path":"/etc/../secret", "bytes":[], "modified_ns":0}]);
    assert!(
        serde_json::from_value::<isoplane_engine::EngineConfig>(encoded).is_err(),
        "deserializing engine configuration must enforce the platform filesystem boundary"
    );
}

#[test]
fn initialization_identity_changes_with_the_declared_trust_snapshot() {
    let wasm = staged_guest("", "", "");
    let mut request = isoplane_prepare::BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: engine_config(),
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        initialization: request(),
        module: isoplane_prepare::Module::new(&wasm),
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
    let wasm = staged_guest("", "", "");
    let mut request = isoplane_prepare::BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: engine_config(),
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        initialization: request(),
        module: isoplane_prepare::Module::new(&wasm),
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_module_without_initialization_retains_its_runtime_table_operations() {
    let wasm = wat::parse_str(
        r#"(module
        (type $mark (func))
        (memory (export "memory") 1 1)
        (table (export "table") 2 funcref)
        (elem (i32.const 1) $mark)
        (func $mark (i32.store8 (i32.const 256) (i32.const 42)))
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (table.set (i32.const 0) (table.get (i32.const 1)))
            (call_indirect (type $mark) (i32.const 0))
            (i64.const 0))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
    )
    .expect("valid runtime table mutation");
    let request = isoplane_prepare::BuildRequest {
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        ..preparation::request(&wasm, engine_config(), request())
    };
    let prepared = preparation::prepare_whole_module(&request)
        .await
        .expect("a module without initialization must retain supported runtime capabilities");
    assert_eq!(
        prepared.memory_after_dispatch()[256],
        42,
        "the dispatch must call the function it stored in the table"
    );
}

#[test]
fn preparation_captures_its_settings_and_changes_identity_when_they_change() {
    let wasm = staged_guest(
        r#"(import "isoplane" "runtime_config_get" (func $settings (param i32 i32) (result i32)))"#,
        "",
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
        let prepared =
            prepare(&wasm, inputs.clone()).expect("settings must be readable during preparation");
        assert_eq!(
            &prepared.memory()[128..128 + settings.len()],
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
fn malformed_preparation_settings_are_refused_without_exposing_values() {
    assert_refused_without_exposing_private_value(b"private-value".to_vec());
}

#[test]
fn oversized_preparation_settings_are_refused_without_exposing_values() {
    let mut settings = br#"{"token":"private-value"}"#.to_vec();
    settings.resize(
        isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES + 1,
        b' ',
    );
    serde_json::from_slice::<BTreeMap<String, String>>(&settings)
        .expect("only the size of these settings is invalid");
    assert_refused_without_exposing_private_value(settings);
}

fn assert_refused_without_exposing_private_value(settings: Vec<u8>) {
    let mut inputs = request();
    inputs.runtime_config = settings;
    let error = refusal(&staged_guest("", "", "unreachable"), inputs);
    assert!(
        matches!(error, PrepareError::InvalidRequest(_)),
        "the settings must be refused before the application's initializer runs: {error:?}"
    );
    let reported = format!("{error} {error:?}");
    assert!(
        !reported.contains("private-value"),
        "the refusal must not repeat the settings: {reported}"
    );
}
