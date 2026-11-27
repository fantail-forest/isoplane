//! The init sandbox: the capability restriction pre-initialisation runs
//! `isoplane_initialize` under.

mod common;
#[path = "common/initialization_bindings.rs"]
mod initialization_bindings;

use common::*;
use initialization_bindings::InitializationTestBindings;
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

/// Bindings for exercising the capability restriction `isoplane_initialize` runs under.
fn init_sandbox(rec: Recorder) -> Box<InitializationTestBindings> {
    Box::new(InitializationTestBindings(TestBindings::new(rec)))
}

#[test]
fn the_init_sandbox_traps_on_a_forbidden_import_and_names_it() {
    let e = test_engine();
    let imports = format!(
        "{}\n{}",
        import("future_cancel", "(param i64)", ""),
        import("log_emit", "(param i32 i32 i32)", ""),
    );
    let wasm = guest(
        4,
        &imports,
        r#"
  (func (export "isoplane_initialize") (result i64)
    (call $log_emit (i32.const 1) (i32.const 0) (i32.const 0))
    (call $future_cancel (i64.const 1)) i64.const 0)"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");

    let rec = Recorder::default();
    let mut i = e
        .activate_initialization(&t, init_sandbox(rec.clone()))
        .expect("activation");
    let out = i.run_initialize(budget(10_000));

    let trap = out
        .trap
        .as_ref()
        .expect("an initializer that cancels a future must trap in the init sandbox");
    assert_eq!(
        trap.kind,
        TrapKind::CapabilityViolation,
        "cancelling a future at init is a capability violation: {trap:?}"
    );
    assert_eq!(
        i.last_capability_violation(),
        Some("future_cancel"),
        "the violation is recorded for preparation to report"
    );

    let calls = rec.calls.lock().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("log_emit")),
        "the init-visible import ran: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("future_cancel")),
        "the forbidden import must trap before reaching a binding: {calls:?}"
    );
}

#[test]
fn a_runtime_configuration_read_reports_the_full_length_and_fills_only_its_capacity() {
    let e = test_engine();
    let wasm = guest(
        4,
        &import("runtime_config_get", "(param i32 i32)", "(result i32)"),
        r#"
  (func (export "isoplane_initialize") (result i64)
    (i32.store (i32.const 600) (call $runtime_config_get (i32.const 0) (i32.const 0)))
    (i32.store (i32.const 604) (call $runtime_config_get (i32.const 1024) (i32.const 8))) i64.const 0)"#,
        DONE,
        DONE,
    );
    let t = e.load_template(&wasm).expect("template");
    let settings: Vec<u8> = (1..=250).cycle().take(9_000).collect();
    let rec = Recorder::default();
    *rec.runtime_configuration.lock() = Some(settings.clone());
    let mut i = e
        .activate_initialization(&t, init_sandbox(rec))
        .expect("activation");
    let out = i.run_initialize(budget(10_000));
    assert_eq!(
        out.completion,
        Completion::Done,
        "the settings read is reachable in the init sandbox: {:?}",
        out.trap
    );

    let m = i.memory();
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
    assert_eq!(
        &m[1024..1032],
        &settings[..8],
        "the capped read must fill exactly `cap` bytes"
    );
    assert_eq!(m[1032], 0, "the capped read must not write past `cap`");
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

struct NativeInitialization {
    permits_io: bool,
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
        Err(isoplane_engine::bindings::BindingError::Unsupported(
            call.import,
        ))
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
fn offline_initialization_reads_the_frozen_coarse_clock() {
    let engine = test_engine();
    let body = r#"(func (export "isoplane_initialize") (result i64)
        (i64.store (i32.const 8) (call $clock_now_coarse))
        (drop (call $burn (i32.const 200000)))
        (i64.store (i32.const 16) (call $clock_now_coarse)) i64.const 0)"#;
    let wasm = guest(
        4,
        &import("clock_now_coarse", "", "(result i64)"),
        body,
        DONE,
        DONE,
    );
    let template = engine.load_template(&wasm).expect("admitted clock import");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut instance = engine
        .activate_initialization(
            &template,
            Box::new(NativeInitialization {
                permits_io: false,
                calls: calls.clone(),
            }),
        )
        .expect("offline initialization context");
    let outcome = instance.run_initialize(budget(10_000));
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "offline initialization must admit the coarse clock: {:?}",
        outcome.trap
    );
    let actual: Vec<_> = instance.memory()[8..24]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| i64::from_le_bytes(*bytes))
        .collect();
    assert_eq!(
        actual,
        [1_234, 1_234],
        "the clock must stay frozen across guest computation"
    );
    assert_eq!(instance.last_capability_violation(), None);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "clock reads must not request native resources"
    );
}

fn check_raw_native_initialization(permits_io: bool, permitted: bool) {
    use isoplane_abi::native_io::canonical::{Action, DEFINITIONS};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let operation = |wanted: fn(&Action) -> bool| {
        DEFINITIONS
            .iter()
            .find(|definition| wanted(&definition.action))
            .expect("the native contract defines the operation")
            .id
            .0
    };
    let network = operation(|action| matches!(action, Action::Network));
    let deadline = operation(|action| matches!(action, Action::Timer { absolute: true }));
    for (imports, call, capability) in [
        (
            r#"(import "isoplane" "socket_call" (func $native (param i32 i32 i32) (result i64)))"#.to_owned(),
            format!("(call $native (i32.const {network}) (i32.const 64) (i32.const 0))"),
            "socket_call",
        ),
        (
            r#"(import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func $native (param i64) (result i32)))"#.to_owned(),
            "(call $native (i64.const 0))".to_owned(),
            "poll_call",
        ),
        (
            r#"(import "isoplane" "poll_call" (func $native (param i32 i32 i32) (result i64)))"#.to_owned(),
            format!("(call $native (i32.const {deadline}) (i32.const 64) (i32.const 1))"),
            "poll_call",
        ),
    ] {
        let engine = test_engine();
        let body = format!(
            r#"(func (export "isoplane_initialize") (result i64)
            (drop {call}) i64.const 0)"#
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
                    permits_io,
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
                "raw native refusals must identify the capability"
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
fn initialization_with_network_authority_permits_raw_native_capabilities() {
    check_raw_native_initialization(true, true);
}

#[test]
fn offline_initialization_refuses_network_and_timer_capabilities() {
    check_raw_native_initialization(false, false);
}
