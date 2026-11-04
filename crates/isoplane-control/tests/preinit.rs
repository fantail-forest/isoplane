//! Pre-initialization in the sealed init sandbox.
//!
//! The guest fixture below is a stand-in for the .NET shape: a
//! `isoplane_initialize` that runs a sequence of *init units* (static
//! constructors), some pure and some not, announcing each one through the
//! interposer so the host can attribute a trap and defer the offender.
//!
//! What is asserted here:
//!  * the sandbox is sealed — a unit that touches a non-init-visible import
//!    traps rather than reading a zero;
//!  * initialization hash seeds and entropy come from the host;
//!  * deferral converges by replay, with the right units deferred and the
//!    pure ones still pre-run;
//!  * the template really carries post-init memory, verified by
//!    re-instantiating it under the *runtime* engine (wasmtime 48) with the
//!    init-only imports bound to traps;
//!  * input identities permit intentional captured-state reuse without forcing random draws to repeat.

use isoplane_abi::config::InitSandbox;
use isoplane_engine::ModuleHash;
use isoplane_prepare::preinit::{PreInitError, PreInitRequest, PreInitializer};

/// Four explicit guest units build a seed-derived table, request a clock,
/// write a high-memory marker and draw application entropy. The clock defers;
/// the allowed state and entropy remain in the successful captured template.
///
/// `impure` controls whether units 2 and 4 exist at all, so the same fixture
/// gives us both the clean and the deferring case.
fn guest_wat(impure: bool) -> String {
    let unit2 = if impure {
        r#"
    ;; ---- unit 2: IMPURE (clock) ----
    (call $unit_begin (i32.const 2))
    (if (i32.eqz (call $unit_deferred (i32.const 2)))
      (then (global.set $clock_at_init (call $now))))"#
    } else {
        ""
    };
    let unit4 = if impure {
        r#"
    ;; ---- unit 4: application entropy ----
    (call $unit_begin (i32.const 4))
    (if (i32.eqz (call $unit_deferred (i32.const 4)))
      (then (call $entropy (i32.const 1024) (i32.const 16))
            (global.set $entropy_at_init (i32.const 1))))"#
    } else {
        ""
    };
    format!(
        r#"(module
  (import "isoplane" "init_hash_seed"     (func $seed_in     (param i32 i32)))
  (import "isoplane" "init_unit_begin"    (func $unit_begin  (param i32)))
  (import "isoplane" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "isoplane" "log_emit"           (func $log         (param i32 i32 i32)))
  (import "isoplane" "clock_now_coarse"   (func $now         (result i64)))
  (import "isoplane" "entropy_fill"       (func $entropy     (param i32 i32)))

  (memory (export "memory") 512 2048)   ;; 32 MiB initial, 128 MiB ceiling

  (global $seed_lo        (mut i64) (i64.const 0))
  (global $table_built    (mut i32) (i32.const 0))
  (global $clock_at_init  (mut i64) (i64.const 0))
  (global $entropy_at_init(mut i32) (i32.const 0))
  (global $high_marker    (mut i32) (i32.const 0))

  (data (i32.const 2048) "pre-init ran")

  (func (export "isoplane_initialize") (result i64)
    (local $i i32)
    ;; Read this attempt's initialization string-hash seed.
    (call $seed_in (i32.const 0) (i32.const 16))
    (global.set $seed_lo (i64.load (i32.const 0)))
    (call $log (i32.const 1) (i32.const 2048) (i32.const 12))

    ;; ---- unit 1: PURE (build a seed-derived table) ----
    (call $unit_begin (i32.const 1))
    (if (i32.eqz (call $unit_deferred (i32.const 1)))
      (then
        (local.set $i (i32.const 65536))
        (block $done (loop $l
          (br_if $done (i32.ge_u (local.get $i) (i32.const 327680)))
          (i64.store (local.get $i)
            (i64.add (global.get $seed_lo) (i64.extend_i32_u (local.get $i))))
          (local.set $i (i32.add (local.get $i) (i32.const 8)))
          (br $l)))
        (global.set $table_built (i32.const 1))))
{unit2}

    ;; ---- unit 3: PURE (a marker 20 MiB up) ----
    (call $unit_begin (i32.const 3))
    (if (i32.eqz (call $unit_deferred (i32.const 3)))
      (then (i64.store (i32.const 20971520) (i64.const 0x1234567890abcdef))
            (global.set $high_marker (i32.const 1))))
{unit4}

    ;; Leave unit scope (NO_UNIT = -1): a trap here is nobody's cctor.
    (call $unit_begin (i32.const -1)) i64.const 0)

  ;; ---- runtime exports ----
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))

  ;; ---- probes, so a test can read the template back ----
  (func (export "probe") (param i32) (result i64) (i64.load (local.get 0)))
  (func (export "get_seed_lo") (result i64) (global.get $seed_lo))
  (func (export "get_table_built") (result i32) (global.get $table_built))
  (func (export "get_clock_at_init") (result i64) (global.get $clock_at_init))
  (func (export "get_entropy_at_init") (result i32) (global.get $entropy_at_init))
  (func (export "get_high_marker") (result i32) (global.get $high_marker))
  ;; Keep an init-only capability callable from a runtime export so the
  ;; test observes runtime-phase refusal. This does not hide values already
  ;; captured in guest memory.
  (func (export "runtime_reread_seed") (call $seed_in (i32.const 32) (i32.const 16)))
)"#
    )
}

fn req(wasm: &[u8]) -> PreInitRequest {
    PreInitRequest {
        module_hash: ModuleHash::of(wasm),

        init_blob: b"{\"greeting\":\"hello\"}".to_vec(),
    }
}

#[test]
fn pure_module_pre_inits_in_one_pass() {
    let wasm = wat::parse_str(guest_wat(false)).unwrap();
    let out = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("pre-init must succeed");

    println!(
        "pure module: {} bytes in -> {} bytes template, {} pass(es), {} ms occupancy, {} refusals",
        wasm.len(),
        out.template_bytes,
        out.passes,
        out.init_occupancy_ms,
        out.capability_refusals
    );
    assert_eq!(out.passes, 1);
    assert!(out.deferred_units.is_empty());
    assert_eq!(out.capability_refusals, 0);
    assert_eq!(
        out.seed_reads, 1,
        "the initializer reads its per-attempt hash seed once"
    );
    assert!(
        out.init_logs.iter().any(|l| l.contains("pre-init ran")),
        "{:?}",
        out.init_logs
    );
}

/// A refused clock defers its explicit guest unit; the successful attempt
/// still captures the other units and application entropy.
#[test]
fn impure_units_are_deferred_and_pre_init_converges() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let out = PreInitializer::new()
        .run(&wasm, &req(&wasm))
        .expect("pre-init must converge");

    println!(
        "impure module: {} passes, deferred {:?}, reasons {:?}, {} refusals, {} ms occupancy",
        out.passes,
        out.deferred_units,
        out.deferred_reasons,
        out.capability_refusals,
        out.init_occupancy_ms
    );
    // The clock is deferred, then a fresh attempt completes.
    assert_eq!(
        out.passes, 2,
        "one pass per discovered impure unit, plus one clean pass"
    );
    assert_eq!(out.deferred_units, vec![2]);
    assert_eq!(
        out.deferred_reasons,
        vec![(2, "clock_now_coarse".into())],
        "each deferred unit must retain the capability that caused its refusal"
    );
    assert_eq!(out.capability_refusals, 1);

    // Allowed state and entropy are captured; the refused clock assignment is not.
    let mut probe = Template::load(&out.template_wasm);
    assert_eq!(
        probe.i32("get_table_built"),
        1,
        "pure unit 1 must have pre-run"
    );
    assert_eq!(
        probe.i32("get_high_marker"),
        1,
        "pure unit 3 must have pre-run"
    );
    assert_eq!(
        probe.i64("get_clock_at_init"),
        0,
        "impure unit 2 must be left un-run"
    );
    assert_eq!(
        probe.i32("get_entropy_at_init"),
        1,
        "entropy is available inside application initialization units"
    );
}

/// The deferral list is not derivable from the wasm, so it rides *inside*
/// the artifact as the `isoplane.deferred-init` custom section — which is what
/// `isoplane-engine::Template::deferred_init_units()` reads.
#[test]
fn the_engine_reads_the_deferred_units_out_of_the_template() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_eq!(out.deferred_units, vec![2], "the clock unit is deferred");
    let engine = isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
        max_instances: 2,
        max_memory_bytes: 128 << 20,
        guard_size_bytes: 2 << 20,
        ..isoplane_engine::EngineConfig::default()
    })
    .expect("engine");
    let template = engine
        .load_template(&out.template_wasm)
        .expect("the template loads into the data plane");
    assert_eq!(
        template.deferred_init_units(),
        &out.deferred_units[..],
        "the deferred units travel inside the template"
    );

    let pure = wat::parse_str(guest_wat(false)).unwrap();
    let clean = PreInitializer::new().run(&pure, &req(&pure)).unwrap();
    assert!(
        engine
            .load_template(&clean.template_wasm)
            .expect("the template loads")
            .deferred_init_units()
            .is_empty(),
        "a pure module defers nothing"
    );
}

/// Loads captured memory and globals in the runtime engine.
#[test]
fn template_carries_post_init_memory_into_the_runtime_engine() {
    let wasm = wat::parse_str(guest_wat(false)).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let mut t = Template::load(&out.template_wasm);

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

    println!(
        "module {} bytes -> template {} bytes ({:.2}x); zstd-19 {} bytes",
        wasm.len(),
        out.template_bytes,
        out.template_bytes as f64 / wasm.len() as f64,
        zstd::encode_all(&out.template_wasm[..], 19).unwrap().len()
    );
}

#[test]
fn fresh_initializations_draw_fresh_seeds_under_the_same_input_identity() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let a = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let b = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_ne!(
        a.template_wasm, b.template_wasm,
        "new initialization must draw fresh entropy"
    );
    assert_eq!(
        a.initialization_identity, b.initialization_identity,
        "the identity describes inputs, not random output"
    );
    assert_eq!(a.deferred_units, b.deferred_units);
    let mut changed = req(&wasm);
    changed.init_blob = b"{}".to_vec();
    assert_ne!(
        a.initialization_identity,
        PreInitializer::new()
            .run(&wasm, &changed)
            .unwrap()
            .initialization_identity
    );
}

/// A trap with no init unit in scope cannot be attributed, and guessing would
/// silently change program behaviour. Fail the deploy instead.
#[test]
fn unattributable_traps_fail_the_deploy() {
    let wat = r#"(module
  (import "isoplane" "init_unit_begin" (func $unit_begin (param i32)))
  (import "isoplane" "clock_now_coarse" (func $now (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (call $unit_begin (i32.const -1))   ;; explicitly outside any unit
    (global.set $g (call $now)) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    assert!(
        matches!(
            err,
            PreInitError::Unattributable {
                ref import,
                ..
            } if import == "clock_now_coarse"
        ),
        "a trap outside any unit names the capability and fails the deploy: {err}"
    );
}

/// The failure mode this whole file exists to avoid: a sealed import must
/// TRAP, not quietly return zero. A zero-returning stub would bake a wrong
/// value (a fixed clock, a fixed "random" number) into every instance.
#[test]
fn sealed_imports_trap_rather_than_returning_zero() {
    // Unit 2 reads the clock but ignores `init_unit_deferred`, so it traps
    // again on the replay pass — and the host says exactly that rather than
    // looping.
    let wat = r#"(module
  (import "isoplane" "init_unit_begin"    (func $unit_begin (param i32)))
  (import "isoplane" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "isoplane" "clock_now_coarse"   (func $now (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (call $unit_begin (i32.const 7))
    (drop (call $unit_deferred (i32.const 7)))   ;; asks, then ignores the answer
    (global.set $g (call $now)) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    assert!(
        matches!(err, PreInitError::DeferralIgnored { unit: 7, .. }),
        "a unit that runs after being told to skip is reported, not replayed forever: {err}"
    );
}

/// Initialization-only imports remain unavailable to an activated instance.
#[test]
fn a_template_cannot_re_read_the_hash_seed_at_runtime() {
    let wasm = wat::parse_str(guest_wat(false)).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let mut t = Template::load(&out.template_wasm);

    let captured = t.probe(0);
    assert_eq!(
        t.i64("get_seed_lo"),
        captured,
        "the hash seed is captured in guest state"
    );

    // ...but the live instance cannot ask for it again.
    let err = t
        .instance
        .get_typed_func::<(), ()>(&mut t.store, "runtime_reread_seed")
        .unwrap()
        .call(&mut t.store, ())
        .expect_err("re-reading the seed at runtime must trap");
    // The host error is the trap's *cause*, so look at the whole chain.
    let chain = format!("{err:?}");
    println!("{chain}");
    assert!(
        chain.contains("called at RUNTIME"),
        "the trap must name the violation, got: {chain}"
    );
}

/// Bring-up mode defers a unit clock while capturing allowed entropy.
#[test]
fn runtime_bringup_keeps_clock_deferral_while_entropy_is_available_in_application_units() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let out = PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(&wasm, &req(&wasm))
        .unwrap();
    assert_eq!(out.deferred_units, vec![2]);
    assert_eq!(out.deferred_reasons, vec![(2, "clock_now_coarse".into())]);
    assert_eq!(out.pal_calls, vec![("entropy_fill".into(), 1)]);
    let mut template = Template::load(&out.template_wasm);
    assert_eq!(template.i64("get_clock_at_init"), 0);
    assert_eq!(template.i32("get_entropy_at_init"), 1);
}

/// Bring-up mode supplies the fixed clock and fresh entropy outside a unit.
#[test]
fn the_runtime_pal_lets_a_runtime_come_up_outside_any_init_unit() {
    let wat = r#"(module
  (import "isoplane" "clock_now_coarse" (func $now (result i64)))
  (import "isoplane" "entropy_fill"     (func $entropy (param i32 i32)))
  (memory (export "memory") 1 16)
  (global $t (mut i64) (i64.const -1))
  ;; No init_unit_begin at all: this is a runtime bringing itself up, which is
  ;; exactly what a .NET module does before any tenant cctor runs.
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

    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    assert!(
        matches!(err, PreInitError::Unattributable { .. }),
        "sealed, a runtime coming up outside any unit fails the deploy: {err}"
    );

    // The clock policy remains frozen; each entropy request uses the live host source.
    let out = PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(&wasm, &req(&wasm))
        .expect("a runtime must be able to come up");
    println!("PAL calls: {:?}", out.pal_calls);
    assert_eq!(
        out.pal_calls,
        vec![
            ("clock_now_coarse".to_string(), 1),
            ("entropy_fill".to_string(), 1)
        ],
        "runtime bring-up must record the clock and entropy answers it consumed"
    );

    let mut t = Template::load(&out.template_wasm);
    assert_eq!(
        t.i64("get_t"),
        0,
        "runtime bring-up supplies the zero initialization clock"
    );
    let entropy = t.probe(0);
    assert_ne!(entropy, 0, "entropy must be real bytes, not zeros");

    let again = PreInitializer::new()
        .with_pal(InitSandbox::RuntimeBringUp)
        .run(&wasm, &req(&wasm))
        .unwrap();
    assert_ne!(out.template_wasm, again.template_wasm);
    assert_ne!(Template::load(&again.template_wasm).probe(0), entropy);
}

/// A runtime-only operation defers the active guest unit before its state assignment.
#[test]
fn a_runtime_timer_defers_its_active_initialization_unit() {
    let wat = r#"(module
  (import "isoplane" "init_unit_begin"    (func $unit_begin (param i32)))
  (import "isoplane" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "isoplane" "clock_deadline_in" (func $deadline (param i64) (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "isoplane_initialize") (result i64)
    (call $unit_begin (i32.const 9))
    (if (i32.eqz (call $unit_deferred (i32.const 9)))
      (then (global.set $g (call $deadline (i64.const 1)))))
    (call $unit_begin (i32.const -1)) i64.const 0)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))
  (func (export "get_g") (result i64) (global.get $g))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_eq!(out.deferred_units, vec![9]);
    assert_eq!(out.deferred_reasons, vec![(9, "clock_deadline_in".into())]);
    // The refused capability defers unit 9 before it can assign the global.
    assert_eq!(Template::load(&out.template_wasm).i64("get_g"), 0);
}

/// The declared-memory gate refuses before a single guest instruction runs.
#[test]
fn init_memory_limit_is_enforced() {
    let big = wat::parse_str(
        r#"(module (memory (export "memory") 16384 16384)
             (func (export "isoplane_initialize") (result i64) i64.const 0))"#,
    )
    .unwrap();
    let err = PreInitializer::new().run(&big, &req(&big)).unwrap_err();
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
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
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
        // Wizer does not strip the import section, so init-only imports are
        // still *declared* in the template. Bind them to traps: if anything
        // at runtime calls one, the test finds out loudly instead of the
        // template quietly not being sealed.
        for def in isoplane_abi::imports::IMPORTS {
            let name = def.name;
            let ty = func_type(&engine, def);
            linker
                .func_new(
                    isoplane_abi::imports::HOST_MODULE,
                    name,
                    ty,
                    move |_c, _a, rets| {
                        if name.starts_with("init_") {
                            wasmtime::bail!("init-only import `{name}` called at RUNTIME");
                        }
                        // These state probes do not execute runtime I/O.
                        for r in rets.iter_mut() {
                            *r = match r {
                                wasmtime::Val::I64(_) => wasmtime::Val::I64(0),
                                _ => wasmtime::Val::I32(0),
                            };
                        }
                        Ok(())
                    },
                )
                .unwrap();
        }
        for import in module
            .imports()
            .filter(|import| import.module() != isoplane_abi::imports::HOST_MODULE)
        {
            let wasmtime::ExternType::Func(signature) = import.ty() else {
                panic!("template has a non-function foreign import");
            };
            linker
                .func_new(
                    import.module(),
                    import.name(),
                    signature,
                    |_caller, _arguments, _results| {
                        wasmtime::bail!("a template inspection must not execute a foreign import")
                    },
                )
                .expect("bind template inspection guard");
        }
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

fn func_type(
    engine: &wasmtime::Engine,
    def: &isoplane_abi::imports::ImportDef,
) -> wasmtime::FuncType {
    use isoplane_abi::imports::Ty;
    let cvt = |t: &Ty| match t {
        Ty::I32 => wasmtime::ValType::I32,
        Ty::I64 => wasmtime::ValType::I64,
    };
    wasmtime::FuncType::new(
        engine,
        def.sig.params.iter().map(cvt),
        def.sig.results.iter().map(cvt),
    )
}

#[test]
fn the_sealed_initializer_preserves_both_filesystem_adapter_routes() {
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
        .expect("deterministic filesystem probes must be admitted by sealed initialization");
    let mut template = Template::load(&initialized.template_wasm);
    for pointer in [128, 256] {
        assert_eq!(
            template.probe(pointer),
            0,
            "both adapter routes must return successful root metadata during sealed initialization"
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
        initialized.template_wasm, repeated.template_wasm,
        "filesystem initialization must not depend on a host directory or resource identity"
    );
}

fn pal_init_guest(module: &str, function: &str, body: &str) -> Vec<u8> {
    let source = guest_wat(false)
        .replacen(
            "(module",
            &format!(r#"(module (import "{module}" "{function}" (func $pal_probe (param i32)))"#),
            1,
        )
        .replace("(local $i i32)", &format!("(local $i i32) {body}"));
    wat::parse_str(source).expect("the PAL initialization fixture must assemble")
}

#[test]
fn sealed_initialization_receives_an_empty_launch_argument_list() {
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
        .expect("argument absence must also be supported during sealed initialization");
    assert_eq!(
        output.passes, 1,
        "empty launch arguments must not defer initialization"
    );
    assert_eq!(
        output.pal_calls,
        vec![(isoplane_abi::pal::CLI_ENV.to_owned(), 1)],
        "the initialization ledger must record the delegated launch argument call"
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
        .expect_err("wall-clock resolution must remain denied during initialization");
    assert!(
        matches!(error, PreInitError::Trapped(_)),
        "initialization must fail with a trapped denied operation: {error:#}"
    );
}
