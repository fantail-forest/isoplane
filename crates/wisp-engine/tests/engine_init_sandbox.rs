//! The init sandbox: the capability restriction pre-initialisation runs
//! `wisp_initialize` under.

mod common;

use common::*;
use wisp_engine::{Completion, CpuBudget, InstanceOps, TrapKind};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

fn budget(cpu_ms: u32) -> CpuBudget {
    CpuBudget {
        cpu_ms,
        wall_ms_cap: 30_000,
        spent_ns: 0,
    }
}

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "wisp" "{name}" (func ${name} {params} {results}))"#)
}

impl TestBindings {
    /// Bindings whose `is_init_sandbox()` is true, for exercising the
    /// capability restriction `wisp_initialize` runs under.
    fn init_sandbox(rec: Recorder) -> Self {
        Self {
            init_sandbox: true,
            ..Self::new(rec)
        }
    }
}

#[test]
fn the_init_sandbox_traps_on_a_forbidden_import_and_names_it() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}\n{}",
        import("clock_now_coarse", "", "(result i64)"),
        import("log_emit", "(param i32 i32 i32)", ""),
        import("init_unit_begin", "(param i32)", ""),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "wisp_initialize")
    (call $init_unit_begin (i32.const 4))
    (i64.store (i32.const 0) (call $clock_now_coarse)))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let rec = Recorder::default();
    let mut i = e
        .activate(&t, Box::new(TestBindings::init_sandbox(rec.clone())))
        .expect("activation");
    let out = i.run_initialize(budget(10_000));

    let trap = out
        .trap
        .as_ref()
        .expect("an impure initializer must trap in the init sandbox");
    assert_eq!(
        trap.kind,
        TrapKind::CapabilityViolation,
        "reading the clock at init is a capability violation: {trap:?}"
    );
    assert!(
        trap.message.contains("clock_now_coarse"),
        "the trap must name the import: {}",
        trap.message
    );
    assert_eq!(
        i.last_capability_violation(),
        Some("clock_now_coarse"),
        "the violation is recorded for the deferral loop"
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("init_unit_begin")),
        "the init-visible import ran: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("clock_now_coarse")),
        "the forbidden import must trap before reaching a binding: {calls:?}"
    );
}

#[test]
fn the_init_sandbox_permits_exactly_the_init_visible_set() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}\n{}",
        import("init_hash_seed", "(param i32 i32)", ""),
        import("init_config_get", "(param i32 i32)", "(result i32)"),
        import("init_unit_deferred", "(param i32)", "(result i32)"),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "wisp_initialize")
    (if (i32.eqz (call $init_unit_deferred (i32.const 1)))
      (then
        (call $init_hash_seed (i32.const 512) (i32.const 16))
        (i32.store (i32.const 600) (call $init_config_get (i32.const 0) (i32.const 0)))
        (i32.store (i32.const 604) (call $init_config_get (i32.const 1024) (i32.const 8))))))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut b = TestBindings::init_sandbox(Recorder::default());
    b.full_length = Some(9_000);
    let mut i = e.activate(&t, Box::new(b)).expect("activation");
    let out = i.run_initialize(budget(10_000));
    assert_eq!(
        out.completion,
        Completion::Done,
        "the init-visible imports are reachable: {:?}",
        out.trap
    );

    let m = i.memory();
    let seed = &m[512..528];
    assert!(
        seed.iter().all(|b| *b == seed[0]) && seed[0] != 0,
        "the hash seed must be delivered into the guest's buffer"
    );
    assert_eq!(
        read_u32(m, 600),
        9_000,
        "a zero-capacity probe must report the full length, not what was copied"
    );
    assert_eq!(
        read_u32(m, 604),
        9_000,
        "a capped read still reports the full length"
    );
    let capped = &m[1024..1032];
    assert!(
        capped.iter().all(|b| *b == capped[0]) && capped[0] != 0,
        "the capped read must fill exactly `cap` bytes"
    );
    assert_eq!(m[1032], 0, "the capped read must not write past `cap`");
    assert_ne!(
        seed[0], capped[0],
        "two host fills in one dispatch must be two draws"
    );

    let visible: Vec<&str> = wisp_abi::imports::init_visible().map(|d| d.name).collect();
    assert!(
        visible.contains(&"init_hash_seed") && visible.contains(&"init_config_get"),
        "the sandbox admits what the table marks init-visible: {visible:?}"
    );
    assert!(
        !visible.contains(&"clock_now_coarse"),
        "the clock is not init-visible: {visible:?}"
    );
}

#[test]
fn outside_the_init_sandbox_every_declared_import_is_reachable() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("clock_now_coarse", "", "(result i64)"),
        r#"(func (export "wisp_initialize") (i64.store (i32.const 0) (call $clock_now_coarse)))"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.run_initialize(CpuBudget::http());
    assert_eq!(
        out.completion,
        Completion::Done,
        "outside the sandbox the clock is reachable: {:?}",
        out.trap
    );
    assert_eq!(
        i.last_capability_violation(),
        None,
        "no violation is recorded outside the sandbox"
    );
}

#[test]
fn runtime_configuration_is_refused_before_the_preparation_binding_runs() {
    let engine = test_engine();
    let wasm = guest(
        4,
        &import("runtime_config_get", "(param i32 i32)", "(result i32)"),
        r#"(func (export "wisp_initialize") (drop (call $runtime_config_get (i32.const 0) (i32.const 0))))"#,
        DONE,
        DONE,
    );
    let template = engine
        .load_template(&wasm)
        .expect("valid runtime configuration import");
    let recorder = Recorder::default();
    let mut instance = engine
        .activate(
            &template,
            Box::new(TestBindings::init_sandbox(recorder.clone())),
        )
        .expect("activation");
    let outcome = instance.run_initialize(budget(10_000));
    assert_eq!(
        outcome
            .trap
            .expect("runtime configuration must trap during preparation")
            .kind,
        TrapKind::CapabilityViolation,
        "preparation must not receive runtime secrets"
    );
    assert!(
        recorder.calls.lock().is_empty(),
        "runtime secrets must be refused before calling the host binding"
    );
}
