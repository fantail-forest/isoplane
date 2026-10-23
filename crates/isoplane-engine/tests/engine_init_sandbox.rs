//! The init sandbox: the capability restriction pre-initialisation runs
//! `isoplane_initialize` under.

mod common;

use common::*;
use isoplane_engine::{Completion, InstanceOps, OccupancyBudget, TrapKind};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

fn budget(occupancy_ms: u32) -> OccupancyBudget {
    OccupancyBudget {
        occupancy_ms,
        slice_wall_ms_cap: 30_000,
        spent_ns: 0,
    }
}

fn import(name: &str, params: &str, results: &str) -> String {
    format!(r#"(import "isoplane" "{name}" (func ${name} {params} {results}))"#)
}

impl TestBindings {
    /// Bindings whose `is_init_sandbox()` is true, for exercising the
    /// capability restriction `isoplane_initialize` runs under.
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
  (func (export "isoplane_initialize") (result i64)
    (call $init_unit_begin (i32.const 4))
    (i64.store (i32.const 0) (call $clock_now_coarse)) i64.const 0)"#,
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
  (func (export "isoplane_initialize") (result i64)
    (if (i32.eqz (call $init_unit_deferred (i32.const 1)))
      (then
        (call $init_hash_seed (i32.const 512) (i32.const 16))
        (i32.store (i32.const 600) (call $init_config_get (i32.const 0) (i32.const 0)))
        (i32.store (i32.const 604) (call $init_config_get (i32.const 1024) (i32.const 8))))) i64.const 0)"#,
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

    let visible: Vec<&str> = isoplane_abi::imports::init_visible()
        .map(|d| d.name)
        .collect();
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
        r#"(func (export "isoplane_initialize") (result i64) (i64.store (i32.const 0) (call $clock_now_coarse)) i64.const 0)"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    let out = i.run_initialize(OccupancyBudget::default());
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
fn runtime_configuration_reaches_the_preparation_binding() {
    let engine = test_engine();
    let wasm = guest(
        4,
        &import("runtime_config_get", "(param i32 i32)", "(result i32)"),
        r#"(func (export "isoplane_initialize") (result i64) (drop (call $runtime_config_get (i32.const 0) (i32.const 0))) i64.const 0)"#,
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
        outcome.completion,
        Completion::Done,
        "initialization must admit deployment settings"
    );
    assert!(
        !recorder.calls.lock().is_empty(),
        "the preparation binding must receive the settings read"
    );
}

struct NativeInitialization {
    bringup: bool,
    permits_io: bool,
    in_unit: bool,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl isoplane_engine::bindings::InitializationBindings for NativeInitialization {
    fn begin_slice(&mut self) {}
    fn permits_io(&self) -> bool {
        self.permits_io
    }
    fn slice_clock_ms(&self) -> u64 {
        1_234
    }
    fn runtime_bringup(&self) -> bool {
        self.bringup && !self.in_unit
    }
    fn native_io(
        &mut self,
        _request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(isoplane_abi::native_io::Response::Handle(
            isoplane_abi::native_io::ResourceHandle(1),
        ))
    }
    fn call_sync(
        &mut self,
        call: isoplane_engine::ImportCall<'_>,
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        if call.import == "init_unit_begin" {
            self.in_unit = true;
            Ok(0)
        } else {
            Err(isoplane_engine::bindings::BindingError::Unsupported(
                call.import,
            ))
        }
    }
    fn call_fill(
        &mut self,
        call: isoplane_engine::ImportCall<'_>,
        _out: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        Err(isoplane_engine::bindings::BindingError::Unsupported(
            call.import,
        ))
    }
}

#[test]
fn offline_runtime_bringup_reads_the_frozen_coarse_clock_through_wasi() {
    check_offline_clock(
        "now",
        r#"
        (i64.store (i32.const 8) (call $clock_now_coarse))
        (i64.store (i32.const 16) (call $wasi_clock))
        (drop (call $burn (i32.const 200000)))
        (i64.store (i32.const 24) (call $wasi_clock))
        (i64.store (i32.const 32) (call $clock_now_coarse))"#,
        &[1_234, 1_234_000_000, 1_234_000_000, 1_234],
    );
}

#[test]
fn offline_runtime_bringup_reports_the_coarse_clock_resolution() {
    check_offline_clock(
        "resolution",
        "(i64.store (i32.const 8) (call $wasi_clock))",
        &[isoplane_abi::pal::CLOCK_QUANTUM_NS],
    );
}

fn check_offline_clock(function: &str, body: &str, expected: &[i64]) {
    let engine = test_engine();
    let imports = format!(
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" "{function}" (func $wasi_clock (result i64)))
        (import "isoplane" "clock_now_coarse" (func $clock_now_coarse (result i64)))"#
    );
    let body = format!(
        r#"(func (export "isoplane_initialize") (result i64)
        {body} i64.const 0)"#
    );
    let wasm = guest(4, &imports, &body, DONE, DONE);
    let template = engine.load_template(&wasm).expect("admitted clock import");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut instance = engine
        .activate_initialization(
            &template,
            Box::new(NativeInitialization {
                bringup: true,
                permits_io: false,
                in_unit: false,
                calls: calls.clone(),
            }),
        )
        .expect("offline initialization context");
    let outcome = instance.run_initialize(budget(10_000));
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "offline runtime bring-up must admit WASI clock {function}: {:?}",
        outcome.trap
    );
    let actual: Vec<_> = instance.memory()[8..8 + expected.len() * 8]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| i64::from_le_bytes(*bytes))
        .collect();
    assert_eq!(
        actual, expected,
        "the WASI clock must retain coarse fidelity"
    );
    assert_eq!(instance.last_capability_violation(), None);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "clock reads must not request native resources"
    );
}

fn check_raw_native_initialization(bringup: bool, unit: bool, permits_io: bool, permitted: bool) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    for (module, function, parameters, argument, capability) in [
        (
            "wasi:sockets/instance-network@0.2.0",
            "instance-network",
            "",
            "",
            "socket_call",
        ),
        (
            "wasi:clocks/monotonic-clock@0.2.0",
            "subscribe-duration",
            "(param i64)",
            "(i64.const 0)",
            "poll_call",
        ),
        (
            "wasi:clocks/monotonic-clock@0.2.0",
            "subscribe-instant",
            "(param i64)",
            "(i64.const 0)",
            "poll_call",
        ),
    ] {
        let engine = test_engine();
        let imports = format!(
            r#"(import "{module}" "{function}" (func $native {parameters} (result i32)))
            (import "isoplane" "init_unit_begin" (func $unit (param i32)))"#
        );
        let enter_unit = if unit {
            "(call $unit (i32.const 7))"
        } else {
            ""
        };
        let body = format!(
            r#"(func (export "isoplane_initialize") (result i64)
            {enter_unit} (drop (call $native {argument})) i64.const 0)"#
        );
        let wasm = guest(4, &imports, &body, DONE, DONE);
        let template = engine
            .load_template(&wasm)
            .expect("admitted raw native import");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut instance = engine
            .activate_initialization(
                &template,
                Box::new(NativeInitialization {
                    bringup,
                    permits_io,
                    in_unit: false,
                    calls: calls.clone(),
                }),
            )
            .expect("initialization context");
        let outcome = instance.run_initialize(budget(10_000));
        if permitted {
            assert_eq!(
                outcome.completion,
                Completion::Done,
                "authorized raw native capability must complete: {:?}",
                outcome.trap
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "authorized raw native capability must reach the host binding"
            );
        } else {
            assert_eq!(
                outcome.trap.expect("capability rejection").kind,
                TrapKind::CapabilityViolation,
                "raw native imports must obey initialization capability restrictions"
            );
            assert_eq!(
                instance.last_capability_violation(),
                Some(capability),
                "raw native refusals must identify the capability for deferral"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "refused raw native capabilities must not reach a host binding"
            );
        }
    }
}

#[test]
fn sealed_initialization_refuses_raw_native_capabilities_even_with_network_authority() {
    check_raw_native_initialization(false, false, true, false);
}

#[test]
fn initialization_units_refuse_raw_native_capabilities_even_with_network_authority() {
    check_raw_native_initialization(true, true, true, false);
}

#[test]
fn authorized_runtime_bringup_permits_raw_native_capabilities_outside_units() {
    check_raw_native_initialization(true, false, true, true);
}

#[test]
fn offline_runtime_bringup_refuses_network_and_timer_capabilities() {
    check_raw_native_initialization(true, false, false, false);
}
