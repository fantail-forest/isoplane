//! Default BouncyCastle random generators cannot replay captured entropy.

#[path = "common/bindings.rs"]
mod bindings;

use std::sync::{atomic::Ordering, OnceLock};

use bindings::{Recorder, TestBindings};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, Instance, InstanceOps, Template, WispEngine,
};

fn runtime() -> &'static (WispEngine, Template) {
    static RUNTIME: OnceLock<(WispEngine, Template)> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 16,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the BC RNG fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_bc_randomness"))
            .expect("the BC RNG fixture must load");
        (engine, template)
    })
}

fn activate(first_byte: u8) -> (Instance, Recorder) {
    let (engine, template) = runtime();
    let recorder = Recorder::default();
    let mut bindings = TestBindings::new(recorder.clone());
    bindings.fill_byte = first_byte;
    let instance = engine
        .activate(template, Box::new(bindings))
        .expect("the BC RNG fixture must activate");
    (instance, recorder)
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

fn dispatch(instance: &mut Instance, path: &str) -> wisp_engine::DispatchOutcome {
    let budget = budget();
    instance.dispatch(
        &DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "GET".into(),
                url: format!("https://randomness.test{path}"),
                path: path.into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: budget.cpu_ms,
            now_ms: 1_700_000_000_000,
        },
        budget,
    )
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    let outcome = dispatch(instance, path);
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the BC RNG handler must finish: {outcome:?}"
    );
    let response = recorder
        .responses
        .lock()
        .pop()
        .expect("the BC RNG handler must respond");
    assert_eq!(response.status, 200, "the BC RNG handler must succeed");
    response.body
}

fn cloned_instances() -> Vec<(Instance, Recorder)> {
    let (mut original, recorder) = activate(0x21);
    let initialized = original.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the BC RNG fixture must initialize"
    );
    assert_eq!(
        request(&mut original, &recorder, "/warm"),
        b"warm",
        "both BC default generators must be initialized before capture"
    );
    request(&mut original, &recorder, "/explicit");
    let state = original
        .capture_mutable_state()
        .expect("the warm BC heap must be capturable");
    let memory = original.memory().to_vec();
    drop(original);
    [0x31, 0x71]
        .into_iter()
        .map(|first_byte| {
            let (mut instance, recorder) = activate(first_byte);
            instance
                .restore_mutable_state(&state)
                .expect("both activations must restore the same captured managed state");
            instance.memory().copy_from_slice(&memory);
            (instance, recorder)
        })
        .collect()
}

fn assert_fresh_cloned_draws(path: &str) {
    let mut results = Vec::new();
    for (mut instance, recorder) in cloned_instances() {
        for _ in 0..2 {
            let before = recorder.entropy_draws.lock().len();
            let body = request(&mut instance, &recorder, path);
            let draws = recorder.entropy_draws.lock();
            assert_eq!(
                draws.len() - before,
                1,
                "each default BC draw must reach its activation's host exactly once"
            );
            assert_eq!(
                draws[before].len(),
                32,
                "the BC probe must request its actual output length"
            );
            let expected = draws[before]
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>();
            assert_eq!(
                body,
                expected.as_bytes(),
                "BC must return the current host draw without replaying captured generator state"
            );
            results.push(body);
        }
    }
    assert_ne!(
        results[0], results[2],
        "two activations must use their distinct prescribed entropy sources"
    );
}

#[test]
fn a_retained_default_bc_generator_draws_from_each_activation_host() {
    assert_fresh_cloned_draws("/default");
}

#[test]
fn the_static_bc_generator_draws_from_each_activation_host() {
    assert_fresh_cloned_draws("/arbitrary");
}

#[test]
fn denied_entropy_cannot_succeed_through_either_bc_default_generator() {
    for path in ["/default", "/arbitrary"] {
        for (mut instance, recorder) in cloned_instances() {
            recorder.deny_entropy.store(true, Ordering::Relaxed);
            let outcome = dispatch(&mut instance, path);
            assert!(
                outcome.completion.tombstones(),
                "denied BC entropy must terminate the guest: {outcome:?}"
            );
            assert!(
                recorder.responses.lock().is_empty(),
                "failed entropy cannot produce a successful BC response"
            );
        }
    }
}

#[test]
fn explicitly_supplied_bc_generators_keep_their_selected_behavior() {
    let mut results = Vec::new();
    for (mut instance, recorder) in cloned_instances() {
        recorder.deny_entropy.store(true, Ordering::Relaxed);
        results.push(request(&mut instance, &recorder, "/explicit"));
        assert!(
            recorder.entropy_draws.lock().is_empty(),
            "an explicitly seeded digest generator must not be replaced with host entropy"
        );
    }
    assert_eq!(
        results[0], results[1],
        "the same explicit seed must preserve deterministic generator behavior"
    );
}
