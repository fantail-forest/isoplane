//! Initialization inputs and captured state.

use wisp_control::preinit::{InitPal, PreInitRequest, PreInitializer};
use wisp_engine::ModuleHash;

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
  (import "wisp" "init_hash_seed"     (func $seed_in     (param i32 i32)))
  (import "wisp" "init_unit_begin"    (func $unit_begin  (param i32)))
  (import "wisp" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "wisp" "log_emit"           (func $log         (param i32 i32 i32)))
  (import "wisp" "clock_now_coarse"   (func $now         (result i64)))
  (import "wisp" "entropy_fill"       (func $entropy     (param i32 i32)))

  (memory (export "memory") 512 2048)   ;; 32 MiB initial, 128 MiB ceiling

  (global $seed_lo        (mut i64) (i64.const 0))
  (global $table_built    (mut i32) (i32.const 0))
  (global $clock_at_init  (mut i64) (i64.const 0))
  (global $entropy_at_init(mut i32) (i32.const 0))
  (global $high_marker    (mut i32) (i32.const 0))

  (data (i32.const 2048) "pre-init ran")

  (func (export "wisp_initialize")
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
    (call $unit_begin (i32.const -1)))

  ;; ---- runtime exports ----
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))

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
        "pure module: {} bytes in -> {} bytes template, {} pass(es), {} ms, {} refusals",
        wasm.len(),
        out.template_bytes,
        out.passes,
        out.init_cpu_ms,
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
        "impure module: {} passes, deferred {:?}, reasons {:?}, {} refusals, {} ms",
        out.passes,
        out.deferred_units,
        out.deferred_reasons,
        out.capability_refusals,
        out.init_cpu_ms
    );
    // The clock is deferred, then a fresh attempt completes.
    assert_eq!(
        out.passes, 2,
        "one pass per discovered impure unit, plus one clean pass"
    );
    assert_eq!(out.deferred_units, vec![2]);
    assert_eq!(out.deferred_reasons, vec![(2, "clock_now_coarse")]);
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

/// §4.4's deferral list is not derivable from the wasm, so it rides *inside*
/// the artifact as the `wisp.deferred-init` custom section — which is what
/// `wisp-engine::Template::deferred_init_units()` reads.
#[test]
fn the_template_carries_its_deferred_units_in_a_custom_section() {
    use wisp_control::preinit::read_deferred_init_section;

    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_eq!(out.deferred_units, vec![2, 4]);
    assert_eq!(
        read_deferred_init_section(&out.template_wasm),
        out.deferred_units,
        "the section must agree with the outcome"
    );
    // The template is still a valid module with the section appended.
    Template::load(&out.template_wasm);

    // Absent section means nothing deferred — a pure module carries no section
    // rather than an empty one.
    let pure = wat::parse_str(guest_wat(false)).unwrap();
    let clean = PreInitializer::new().run(&pure, &req(&pure)).unwrap();
    assert!(clean.deferred_units.is_empty());
    assert!(read_deferred_init_section(&clean.template_wasm).is_empty());
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

/// Fresh preparations retain independent randomness and bind configuration inputs.
#[test]
fn fresh_preparations_capture_randomness_and_bind_configuration() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let a = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    let b = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_ne!(
        a.template_wasm, b.template_wasm,
        "fresh preparations capture fresh initialization randomness"
    );
    assert_eq!(a.reproducible_digest, b.reproducible_digest);
    assert_eq!(a.deferred_units, b.deferred_units);

    // ...and so is a different init blob.
    let mut third = req(&wasm);
    third.init_blob = b"{}".to_vec();
    assert_ne!(
        a.reproducible_digest,
        PreInitializer::new()
            .run(&wasm, &third)
            .unwrap()
            .reproducible_digest
    );
}

/// A trap with no init unit in scope cannot be attributed, and guessing would
/// silently change program behaviour. Fail the deploy instead.
#[test]
fn unattributable_traps_fail_the_deploy() {
    let wat = r#"(module
  (import "wisp" "init_unit_begin" (func $unit_begin (param i32)))
  (import "wisp" "clock_now_coarse" (func $now (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "wisp_initialize")
    (call $unit_begin (i32.const -1))   ;; explicitly outside any unit
    (global.set $g (call $now)))
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    println!("{err}");
    assert!(err.to_string().contains("outside any init unit"));
    assert!(err.to_string().contains("clock_now_coarse"));
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
  (import "wisp" "init_unit_begin"    (func $unit_begin (param i32)))
  (import "wisp" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "wisp" "clock_now_coarse"   (func $now (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "wisp_initialize")
    (call $unit_begin (i32.const 7))
    (drop (call $unit_deferred (i32.const 7)))   ;; asks, then ignores the answer
    (global.set $g (call $now)))
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    println!("{err}");
    assert!(
        err.to_string().contains("ignoring `init_unit_deferred`"),
        "{err}"
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
fn runtime_bringup_defers_unit_clocks_and_captures_entropy() {
    let wasm = wat::parse_str(guest_wat(true)).unwrap();
    let out = PreInitializer::new()
        .with_pal(InitPal::RuntimeBringUp)
        .run(&wasm, &req(&wasm))
        .expect("pre-init must converge");

    assert_eq!(out.deferred_units, vec![2]);
    assert_eq!(out.deferred_reasons, vec![(2, "clock_now_coarse")]);
    assert_eq!(out.pal_calls, vec![("entropy_fill", 1)]);

    let mut t = Template::load(&out.template_wasm);
    assert_eq!(
        t.i64("get_clock_at_init"),
        0,
        "a frozen clock must NOT have been baked"
    );
    assert_eq!(t.i32("get_entropy_at_init"), 1);
}

/// Bring-up mode supplies the fixed clock and fresh entropy outside a unit.
#[test]
fn the_runtime_pal_lets_a_runtime_come_up_outside_any_init_unit() {
    let wat = r#"(module
  (import "wisp" "clock_now_coarse" (func $now (result i64)))
  (import "wisp" "entropy_fill"     (func $entropy (param i32 i32)))
  (memory (export "memory") 1 16)
  (global $t (mut i64) (i64.const -1))
  ;; No init_unit_begin at all: this is a runtime bringing itself up, which is
  ;; exactly what a .NET module does before any tenant cctor runs.
  (func (export "wisp_initialize")
    (global.set $t (call $now))
    (call $entropy (i32.const 0) (i32.const 32)))
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))
  (func (export "get_t") (result i64) (global.get $t))
  (func (export "probe") (param i32) (result i64) (i64.load (local.get 0)))
)"#;
    let wasm = wat::parse_str(wat).unwrap();

    // The default mode refuses an unscoped clock request.
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    assert!(err.to_string().contains("outside any init unit"), "{err}");

    let out = PreInitializer::new()
        .with_pal(InitPal::RuntimeBringUp)
        .run(&wasm, &req(&wasm))
        .expect("a runtime must be able to come up");
    println!("PAL calls: {:?}", out.pal_calls);
    assert_eq!(
        out.pal_calls,
        vec![("clock_now_coarse", 1), ("entropy_fill", 1)]
    );

    let mut t = Template::load(&out.template_wasm);
    assert_eq!(
        t.i64("get_t"),
        0,
        "runtime bring-up supplies the zero initialization clock"
    );
    let entropy = t.probe(0);
    assert_ne!(entropy, 0, "entropy must be real bytes, not zeros");
}

/// A runtime-only operation defers the active guest unit before its state assignment.
#[test]
fn a_runtime_timer_defers_its_active_initialization_unit() {
    let wat = r#"(module
  (import "wisp" "init_unit_begin"    (func $unit_begin (param i32)))
  (import "wisp" "init_unit_deferred" (func $unit_deferred (param i32) (result i32)))
  (import "wisp" "clock_deadline_in" (func $deadline (param i64) (result i64)))
  (memory (export "memory") 1 16)
  (global $g (mut i64) (i64.const 0))
  (func (export "wisp_initialize")
    (call $unit_begin (i32.const 9))
    (if (i32.eqz (call $unit_deferred (i32.const 9)))
      (then (global.set $g (call $deadline (i64.const 1)))))
    (call $unit_begin (i32.const -1)))
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))
  (func (export "get_g") (result i64) (global.get $g))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let out = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap();
    assert_eq!(out.deferred_units, vec![9]);
    assert_eq!(out.deferred_reasons, vec![(9, "clock_deadline_in")]);
    // The refused capability defers unit 9 before it can assign the global.
    assert_eq!(Template::load(&out.template_wasm).i64("get_g"), 0);
}

/// CPU usage is checked after initialization; declared memory is checked before it.
#[test]
fn init_cpu_limit_is_enforced() {
    // ~10^8 loop iterations: comfortably over a 1 ms budget, comfortably
    // under a test timeout.
    let wat = r#"(module
  (memory (export "memory") 1 16)
  (global $sink (mut i64) (i64.const 0))
  (func (export "wisp_initialize") (local $i i32)
    (local.set $i (i32.const 0))
    (block $done (loop $l
      (br_if $done (i32.ge_u (local.get $i) (i32.const 100000000)))
      (global.set $sink (i64.add (global.get $sink) (i64.const 1)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l))))
  (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1))
)"#;
    let wasm = wat::parse_str(wat).unwrap();
    let err = PreInitializer::with_limits(1, 512 << 20)
        .run(&wasm, &req(&wasm))
        .unwrap_err();
    println!("{err}");
    assert!(err.to_string().contains("CPU limit"));

    // The memory gate refuses before a single guest instruction runs.
    let big = wat::parse_str(
        r#"(module (memory (export "memory") 16384 16384)
             (func (export "wisp_initialize")))"#,
    )
    .unwrap();
    let err = PreInitializer::new().run(&big, &req(&big)).unwrap_err();
    println!("{err}");
    assert!(err.to_string().contains("exceeds the initialization limit"));
}

#[test]
fn a_module_without_an_init_func_is_not_pre_initialized() {
    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1 16)
             (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0)))"#,
    )
    .unwrap();
    let err = PreInitializer::new().run(&wasm, &req(&wasm)).unwrap_err();
    assert!(err.to_string().contains("wisp_initialize"), "{err}");
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
        for def in wisp_abi::imports::IMPORTS {
            let name = def.name;
            let ty = func_type(&engine, def);
            linker
                .func_new(
                    wisp_abi::imports::HOST_MODULE,
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

fn func_type(engine: &wasmtime::Engine, def: &wisp_abi::imports::ImportDef) -> wasmtime::FuncType {
    use wisp_abi::imports::Ty;
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
