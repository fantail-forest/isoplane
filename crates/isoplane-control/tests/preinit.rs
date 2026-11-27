//! Initialization in the init sandbox, and the template it leaves.
//!
//! What is asserted here:
//!  * a forbidden import traps rather than reading a zero;
//!  * initialization entropy comes from the host;
//!  * the template carries post-init memory and globals, read from the
//!    template a node would load;
//!  * input identities permit intentional captured-state reuse without forcing random draws to repeat.

use isoplane_engine::EngineConfig;
use isoplane_harness::preparation::{self, Prepared};
use isoplane_prepare::{BuildRequest, Initialization, PrepareError};

/// Builds a table from drawn entropy, writes a high-memory marker and draws
/// application entropy during initialization.
fn guest_wat() -> String {
    r#"(module
  (import "isoplane" "log_emit"           (func $log         (param i32 i32 i32)))
  (import "isoplane" "entropy_fill"       (func $entropy     (param i32 i32)))

  (memory (export "memory") 512 2048)   ;; 32 MiB initial, 128 MiB ceiling

  (global $table_base      (export "table_base")      (mut i64) (i64.const 0))
  (global $table_built     (export "table_built")     (mut i32) (i32.const 0))
  (global $entropy_at_init (export "entropy_at_init") (mut i32) (i32.const 0))
  (global $high_marker     (export "high_marker")     (mut i32) (i32.const 0))

  (data (i32.const 2048) "pre-init ran")

  (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (local $i i32)
    (call $entropy (i32.const 0) (i32.const 16))
    (global.set $table_base (i64.load (i32.const 0)))
    (call $log (i32.const 1) (i32.const 2048) (i32.const 12))

    (local.set $i (i32.const 65536))
    (block $done (loop $l
      (br_if $done (i32.ge_u (local.get $i) (i32.const 327680)))
      (i64.store (local.get $i)
        (i64.add (global.get $table_base) (i64.extend_i32_u (local.get $i))))
      (local.set $i (i32.add (local.get $i) (i32.const 8)))
      (br $l)))
    (global.set $table_built (i32.const 1))

    ;; A marker 20 MiB up.
    (i64.store (i32.const 20971520) (i64.const 0x1234567890abcdef))
    (global.set $high_marker (i32.const 1))

    (call $entropy (i32.const 1024) (i32.const 16))
    (global.set $entropy_at_init (i32.const 1))
    i64.const 0)

  ;; ---- runtime exports ----
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 8192))
  (func (export "isoplane_handle") (param i32 i32) (result i64)
    (i64.store (i32.const 4096) (global.get $table_base))
    (i64.store (i32.const 4104) (i64.extend_i32_u (global.get $table_built)))
    (i64.store (i32.const 4112) (i64.extend_i32_u (global.get $entropy_at_init)))
    (i64.store (i32.const 4120) (i64.extend_i32_u (global.get $high_marker)))
    (i64.const 0))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#
    .to_string()
}

/// Where the guest's `isoplane_handle` stores each of its globals, as a
/// 64-bit word.
const TABLE_BASE: usize = 4096;
const TABLE_BUILT: usize = 4104;
const ENTROPY_AT_INIT: usize = 4112;
const HIGH_MARKER: usize = 4120;

/// The request for `wasm` as an application on a shared runtime, under the
/// default initialization budgets.
fn request(wasm: &[u8]) -> BuildRequest {
    let engine = EngineConfig {
        max_instances: 4,
        ..EngineConfig::default()
    };
    preparation::request(wasm, engine, Initialization::new(b"{}".to_vec()))
}

/// Prepares `wasm` the way a node prepares an application on a shared runtime.
fn prepare(wasm: &[u8]) -> Result<Prepared, PrepareError> {
    preparation::prepare(&request(wasm))
}

/// The little-endian 64-bit word at `offset` of `memory`.
fn word(memory: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(memory[offset..offset + 8].try_into().expect("eight bytes"))
}

#[test]
fn initialization_reports_its_logs_and_entropy_draws_and_captures_what_it_built() {
    let wasm = wat::parse_str(guest_wat()).unwrap();
    let out = prepare(&wasm).expect("pre-init must succeed");

    assert!(
        out.initialized
            .logs
            .iter()
            .any(|l| l.contains("pre-init ran")),
        "{:?}",
        out.initialized.logs
    );
    assert_eq!(
        out.initialized.pal_calls,
        vec![isoplane_engine::PalCallCount {
            import: "entropy_fill".into(),
            calls: 2,
        }],
        "initialization records the entropy it was answered"
    );

    let instance = out.memory_after_dispatch();
    assert_eq!(
        word(&instance, TABLE_BUILT),
        1,
        "the table built during initialization is captured"
    );
    assert_eq!(
        word(&instance, HIGH_MARKER),
        1,
        "the high-memory marker is captured"
    );
    assert_eq!(
        word(&instance, ENTROPY_AT_INIT),
        1,
        "entropy is available to application initialization"
    );
}

#[test]
fn template_carries_post_init_memory_into_the_runtime_engine() {
    let wasm = wat::parse_str(guest_wat()).unwrap();
    let t = prepare(&wasm).unwrap();

    let memory = t.memory();
    let table_base = word(&memory, 0);
    assert_eq!(
        word(&t.memory_after_dispatch(), TABLE_BASE),
        table_base,
        "the drawn table base is retained in its global"
    );
    assert_eq!(
        word(&memory, 65_536),
        table_base.wrapping_add(65_536),
        "low table page captured"
    );
    assert_eq!(
        word(&memory, 327_672),
        table_base.wrapping_add(327_672),
        "table tail captured"
    );
    assert_eq!(
        word(&memory, 20_971_520),
        0x1234567890abcdef,
        "20 MiB marker survives"
    );
}

/// Initialization is answered a fixed clock and fresh entropy.
#[test]
fn initialization_is_answered_a_fixed_clock_and_fresh_entropy() {
    let wat = r#"(module
  (import "isoplane" "clock_now_coarse" (func $now (result i64)))
  (import "isoplane" "entropy_fill"     (func $entropy (param i32 i32)))
  (memory (export "memory") 1 16)
  (global $t (export "t") (mut i64) (i64.const -1))
  (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (global.set $t (call $now))
    (call $entropy (i32.const 0) (i32.const 32)) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
  (func (export "isoplane_handle") (param i32 i32) (result i64)
    (i64.store (i32.const 64) (global.get $t)) (i64.const 0))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();

    // The clock policy remains frozen; each entropy request uses the live host source.
    let out = prepare(&wasm).expect("initialization may read the clock and draw entropy");
    assert_eq!(
        out.initialized.pal_calls,
        vec![
            isoplane_engine::PalCallCount {
                import: "clock_now_coarse".into(),
                calls: 1,
            },
            isoplane_engine::PalCallCount {
                import: "entropy_fill".into(),
                calls: 1,
            },
        ],
        "initialization must record the clock and entropy answers it consumed"
    );

    assert_eq!(
        word(&out.memory_after_dispatch(), 64),
        0,
        "initialization without network authority reads a zero clock"
    );
    let entropy = word(&out.memory(), 0);
    assert_ne!(entropy, 0, "entropy must be real bytes, not zeros");

    let again = prepare(&wasm).unwrap();
    assert_ne!(word(&again.memory(), 0), entropy);
}

/// A timer is a serving capability.
#[test]
fn a_timer_during_initialization_fails_the_deploy_and_is_named() {
    let wat = r#"(module
  (import "isoplane" "clock_deadline_in" (func $deadline (param i64) (result i64)))
  (memory (export "memory") 1 16)
  (global $g (export "g") (mut i64) (i64.const 0))
  (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (global.set $g (call $deadline (i64.const 1))) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = preparation::application_refusal(&request(&wasm));
    assert!(
        matches!(
            err,
            PrepareError::ForbiddenCapability {
                ref import,
                ..
            } if import == "clock_deadline_in"
        ),
        "a timer during initialization names the capability and fails the deploy: {err}"
    );
}

#[test]
fn the_initializer_preserves_filesystem_adapter_results() {
    let wasm = wat::parse_str(format!(
        r#"(module
      (import "isoplane" "filesystem_call" (func $filesystem (param i32 i32 i32)))
      (memory (export "memory") 1 16)
      (global $heap (export "heap") (mut i32) (i32.const 4096))
      (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
        (local $p i32)
        (local.set $p (global.get $heap))
        (global.set $heap (i32.add (global.get $heap) (i32.const 128)))
        (local.get $p))
      (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
      (func (export "isoplane_initialize") (result i64)
        (i64.store (i32.const 512) (i64.const 16))
        (call $filesystem (i32.const {directories}) (i32.const 512) (i32.const 1))
        (i64.store (i32.const 512) (i64.extend_i32_u (i32.load (i32.load (i32.const 16)))))
        (i64.store (i32.const 520) (i64.const 256))
        (call $filesystem (i32.const {stat}) (i32.const 512) (i32.const 2)) i64.const 0)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
    )"#,
        directories = isoplane_abi::filesystem::Operation::Directories as u32,
        stat = isoplane_abi::filesystem::Operation::Stat as u32
    ))
    .expect("filesystem initialization fixture must compile");
    let initialized =
        prepare(&wasm).expect("deterministic filesystem probes must be admitted by initialization");
    let memory = initialized.memory();
    assert_eq!(
        word(&memory, 256),
        0,
        "the adapter must return successful root metadata during initialization"
    );
    assert_eq!(
        word(&memory, 264),
        3,
        "the initializer must preserve the synthetic directory type"
    );
    let repeated = prepare(&wasm).expect("repeat filesystem initialization");
    assert_eq!(
        (memory, initialized.globals()),
        (repeated.memory(), repeated.globals()),
        "filesystem initialization must not depend on a host directory or resource identity"
    );
}
