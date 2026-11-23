//! Initialization in the init sandbox, and the template it leaves.
//!
//! What is asserted here:
//!  * a forbidden import traps rather than reading a zero;
//!  * initialization hash seeds and entropy come from the host;
//!  * the template carries post-init memory, verified by re-instantiating it
//!    under the runtime engine with its imports bound to traps;
//!  * input identities permit intentional captured-state reuse without forcing random draws to repeat.

use isoplane_engine::ModuleHash;
use isoplane_prepare::preinit::{PreInitError, PreInitRequest, PreInitializer};

/// Builds a seed-derived table, writes a high-memory marker and draws
/// application entropy during initialization.
fn guest_wat() -> String {
    r#"(module
  (import "isoplane" "init_hash_seed"     (func $seed_in     (param i32 i32)))
  (import "isoplane" "log_emit"           (func $log         (param i32 i32 i32)))
  (import "isoplane" "entropy_fill"       (func $entropy     (param i32 i32)))

  (memory (export "memory") 512 2048)   ;; 32 MiB initial, 128 MiB ceiling

  (global $seed_lo        (mut i64) (i64.const 0))
  (global $table_built    (mut i32) (i32.const 0))
  (global $entropy_at_init(mut i32) (i32.const 0))
  (global $high_marker    (mut i32) (i32.const 0))

  (data (i32.const 2048) "pre-init ran")

  (func (export "isoplane_initialize") (result i64)
    (local $i i32)
    ;; Read this attempt's initialization string-hash seed.
    (call $seed_in (i32.const 0) (i32.const 16))
    (global.set $seed_lo (i64.load (i32.const 0)))
    (call $log (i32.const 1) (i32.const 2048) (i32.const 12))

    (local.set $i (i32.const 65536))
    (block $done (loop $l
      (br_if $done (i32.ge_u (local.get $i) (i32.const 327680)))
      (i64.store (local.get $i)
        (i64.add (global.get $seed_lo) (i64.extend_i32_u (local.get $i))))
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
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))

  ;; ---- probes, so a test can read the template back ----
  (func (export "probe") (param i32) (result i64) (i64.load (local.get 0)))
  (func (export "get_seed_lo") (result i64) (global.get $seed_lo))
  (func (export "get_table_built") (result i32) (global.get $table_built))
  (func (export "get_entropy_at_init") (result i32) (global.get $entropy_at_init))
  (func (export "get_high_marker") (result i32) (global.get $high_marker))
)"#
    .to_string()
}

fn req(wasm: &[u8]) -> PreInitRequest {
    PreInitRequest {
        module_hash: ModuleHash::of(wasm),

        init_blob: b"{\"greeting\":\"hello\"}".to_vec(),
    }
}

#[test]
fn initialization_reports_its_seed_read_and_logs_and_captures_what_it_built() {
    let wasm = wat::parse_str(guest_wat()).unwrap();
    let out = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("pre-init must succeed");

    assert_eq!(
        out.initialized.seed_reads, 1,
        "the initializer reads its hash seed once"
    );
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
        vec![("entropy_fill".into(), 1)],
        "initialization records the entropy it was answered"
    );

    let mut probe = Template::load(&out.wasm);
    assert_eq!(
        probe.i32("get_table_built"),
        1,
        "the table built during initialization is captured"
    );
    assert_eq!(
        probe.i32("get_high_marker"),
        1,
        "the high-memory marker is captured"
    );
    assert_eq!(
        probe.i32("get_entropy_at_init"),
        1,
        "entropy is available to application initialization"
    );
}

/// Loads captured memory and globals in the runtime engine.
#[test]
fn template_carries_post_init_memory_into_the_runtime_engine() {
    let wasm = wat::parse_str(guest_wat()).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let mut t = Template::load(&out.wasm);

    let seed_lo = t.probe(0);
    assert_eq!(
        t.i64("get_seed_lo"),
        seed_lo,
        "the captured seed is retained in its global"
    );
    assert_eq!(
        t.probe(65_536),
        seed_lo.wrapping_add(65_536),
        "low table page baked"
    );
    assert_eq!(
        t.probe(327_672),
        seed_lo.wrapping_add(327_672),
        "table tail baked"
    );
    assert_eq!(
        t.probe(20_971_520),
        0x1234567890abcdefu64 as i64,
        "20 MiB marker survives"
    );
}

#[test]
fn fresh_initializations_of_the_same_input_draw_fresh_seeds() {
    let wasm = wat::parse_str(guest_wat()).unwrap();
    let a = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let b = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_ne!(a.wasm, b.wasm, "new initialization must draw fresh entropy");
}

/// Initialization is answered a fixed clock and fresh entropy.
#[test]
fn initialization_is_answered_a_fixed_clock_and_fresh_entropy() {
    let wat = r#"(module
  (import "isoplane" "clock_now_coarse" (func $now (result i64)))
  (import "isoplane" "entropy_fill"     (func $entropy (param i32 i32)))
  (memory (export "memory") 1 16)
  (global $t (mut i64) (i64.const -1))
  (func (export "isoplane_initialize") (result i64)
    (global.set $t (call $now))
    (call $entropy (i32.const 0) (i32.const 32)) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
  (func (export "get_t") (result i64) (global.get $t))
  (func (export "probe") (param i32) (result i64) (i64.load (local.get 0)))
)"#;
    let wasm = wat::parse_str(wat).unwrap();

    // The clock policy remains frozen; each entropy request uses the live host source.
    let out = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("initialization may read the clock and draw entropy");
    println!("PAL calls: {:?}", out.initialized.pal_calls);
    assert_eq!(
        out.initialized.pal_calls,
        vec![
            ("clock_now_coarse".to_string(), 1),
            ("entropy_fill".to_string(), 1)
        ],
        "initialization must record the clock and entropy answers it consumed"
    );

    let mut t = Template::load(&out.wasm);
    assert_eq!(
        t.i64("get_t"),
        0,
        "initialization without network authority reads a zero clock"
    );
    let entropy = t.probe(0);
    assert_ne!(entropy, 0, "entropy must be real bytes, not zeros");

    let again = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_ne!(out.wasm, again.wasm);
    assert_ne!(Template::load(&again.wasm).probe(0), entropy);
}

/// A timer is a serving capability.
#[test]
fn a_timer_during_initialization_fails_the_deploy_and_is_named() {
    let wat = r#"(module
  (import "isoplane" "clock_deadline_in" (func $deadline (param i64) (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (global.set $g (call $deadline (i64.const 1))) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .err()
        .expect("initialization must be refused");
    assert!(
        matches!(
            err,
            PreInitError::ForbiddenCapability {
                ref import,
                ..
            } if import == "clock_deadline_in"
        ),
        "a timer during initialization names the capability and fails the deploy: {err}"
    );
}

/// The declared-memory gate refuses before a single guest instruction runs.
#[test]
fn init_memory_limit_is_enforced() {
    let big = wat::parse_str(
        r#"(module (memory (export "memory") 16384 16384)
             (func (export "isoplane_initialize") (result i64) i64.const 0))"#,
    )
    .unwrap();
    let err = PreInitializer::new()
        .run(&big, &req(&big))
        .err()
        .expect("initialization must be refused");
    assert!(
        matches!(err, PreInitError::MemoryLimit { .. }),
        "a gibibyte of declared memory must be refused: {err}"
    );
}

#[test]
fn a_module_without_an_init_func_is_not_pre_initialized() {
    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1 16)
             (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    let err = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .err()
        .expect("initialization must be refused");
    assert!(
        matches!(err, PreInitError::NoInitFunc("isoplane_initialize")),
        "nothing to pre-initialize: {err}"
    );
}

// ---------------------------------------------------------------------------
// loading a template under the RUNTIME engine (wasmtime 48)
// ---------------------------------------------------------------------------

struct Template {
    store: wasmtime::Store<()>,
    instance: wasmtime::Instance,
}

impl Template {
    fn load(template_wasm: &[u8]) -> Self {
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, template_wasm).expect("template must compile");
        let mut store = wasmtime::Store::new(&engine, ());
        let mut linker: wasmtime::Linker<()> = wasmtime::Linker::new(&engine);
        // Wizer keeps the import section, so the template still declares its
        // imports. The probes these tests call only read guest state.
        linker
            .define_unknown_imports_as_traps(&module)
            .expect("bind template inspection guard");
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("template must instantiate");
        Template { store, instance }
    }

    fn i64(&mut self, name: &str) -> i64 {
        self.instance
            .get_typed_func::<(), i64>(&mut self.store, name)
            .unwrap()
            .call(&mut self.store, ())
            .unwrap()
    }
    fn i32(&mut self, name: &str) -> i32 {
        self.instance
            .get_typed_func::<(), i32>(&mut self.store, name)
            .unwrap()
            .call(&mut self.store, ())
            .unwrap()
    }
    fn probe(&mut self, addr: i32) -> i64 {
        self.instance
            .get_typed_func::<i32, i64>(&mut self.store, "probe")
            .unwrap()
            .call(&mut self.store, addr)
            .unwrap()
    }
}

#[test]
fn the_initializer_preserves_both_filesystem_adapter_routes() {
    let wasm = wat::parse_str(format!(
        r#"(module
      (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $directories (param i32)))
      (import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func $stat (param i32 i32)))
      (import "isoplane" "filesystem_call" (func $filesystem (param i32 i32 i32)))
      (memory (export "memory") 1 16)
      (global $heap (mut i32) (i32.const 4096))
      (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
        (local $p i32)
        (local.set $p (global.get $heap))
        (global.set $heap (i32.add (global.get $heap) (i32.const 128)))
        (local.get $p))
      (func (export "isoplane_initialize") (result i64)
        (call $directories (i32.const 16))
        (call $stat (i32.load (i32.load (i32.const 16))) (i32.const 128))
        (i64.store (i32.const 512) (i64.extend_i32_u (i32.load (i32.load (i32.const 16)))))
        (i64.store (i32.const 520) (i64.const 256))
        (call $filesystem (i32.const {}) (i32.const 512) (i32.const 2)) i64.const 0)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "probe") (param $pointer i32) (result i64) (i64.load (local.get $pointer)))
    )"#,
        isoplane_abi::filesystem::Operation::Stat as u32
    ))
    .expect("filesystem initialization fixture must compile");
    let initialized = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("deterministic filesystem probes must be admitted by initialization");
    let mut template = Template::load(&initialized.wasm);
    for pointer in [128, 256] {
        assert_eq!(
            template.probe(pointer),
            0,
            "both adapter routes must return successful root metadata during initialization"
        );
        assert_eq!(
            template.probe(pointer + 8),
            3,
            "the initializer must preserve the synthetic directory type"
        );
    }
    let repeated = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("repeat filesystem initialization");
    assert_eq!(
        initialized.wasm, repeated.wasm,
        "filesystem initialization must not depend on a host directory or resource identity"
    );
}

fn pal_init_guest(module: &str, function: &str, body: &str) -> Vec<u8> {
    let source = guest_wat()
        .replacen(
            "(module",
            &format!(r#"(module (import "{module}" "{function}" (func $pal_probe (param i32)))"#),
            1,
        )
        .replace("(local $i i32)", &format!("(local $i i32) {body}"));
    wat::parse_str(source).expect("the PAL initialization fixture must assemble")
}

#[test]
fn initialization_receives_an_empty_launch_argument_list() {
    let wasm = pal_init_guest(
        isoplane_abi::pal::CLI_ENV,
        "get-arguments",
        r#"
      (i64.store (i32.const 64) (i64.const -1))
      (call $pal_probe (i32.const 64))
      (if (i64.ne (i64.load (i32.const 64)) (i64.const 0)) (then unreachable))"#,
    );
    let output = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("argument absence must also be supported during initialization");
    assert_eq!(
        output.initialized.pal_calls,
        vec![
            ("entropy_fill".to_owned(), 1),
            (isoplane_abi::pal::CLI_ENV.to_owned(), 1)
        ],
        "the initialization ledger must record the delegated launch argument call beside the fixture's entropy draw"
    );
}

#[test]
fn initialization_rejects_an_invalid_argument_return_pointer() {
    let wasm = pal_init_guest(
        isoplane_abi::pal::CLI_ENV,
        "get-arguments",
        "(call $pal_probe (i32.const -1))",
    );
    assert!(
        PreInitializer::new().run(&wasm, &req(&wasm)).is_err(),
        "the init adapter must reject a return record outside guest memory"
    );
}

#[test]
fn initialization_cannot_query_a_denied_wall_clock_resolution() {
    let wasm = pal_init_guest(
        isoplane_abi::pal::WALL_CLOCK,
        "resolution",
        "(call $pal_probe (i32.const 64))",
    );
    let error = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .err()
        .expect("wall-clock resolution must remain denied during initialization");
    assert!(
        matches!(error, PreInitError::Trapped(_)),
        "initialization must fail with a trapped denied operation: {error:#}"
    );
}
