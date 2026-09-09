//! Ordinary managed static RNG calls use host entropy without cloned PRNG state.

#[path = "common/bindings.rs"]
mod bindings;

use std::sync::OnceLock;

use bindings::{Recorder, TestBindings};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, Instance, InstanceOps, Template, WispEngine,
};

fn runtime() -> &'static (WispEngine, Template) {
    static RUNTIME: OnceLock<(WispEngine, Template)> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 8,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the managed RNG fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_randomness"))
            .expect("the managed RNG fixture must load");
        (engine, template)
    })
}

fn activate(first_byte: u8) -> (Instance, Recorder) {
    let (engine, template) = runtime();
    let recorder = Recorder::default();
    let mut bindings = TestBindings::new(recorder.clone());
    bindings.fill_byte = first_byte;
    let mut instance = engine
        .activate(template, Box::new(bindings))
        .expect("the managed RNG fixture must activate");
    let initialized = instance.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the managed RNG fixture must initialize: {initialized:?}"
    );
    // Warm managed response/exception infrastructure so its own entropy is not
    // mistaken for an explicit RNG call in the measured request.
    assert_eq!(
        request(&mut instance, &recorder, "/empty"),
        b"0",
        "empty RNG warmup must succeed"
    );
    assert_eq!(
        request(&mut instance, &recorder, "/negative"),
        b"ArgumentOutOfRangeException:count:-1",
        "negative-count warmup must preserve argument behavior"
    );
    (instance, recorder)
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    let outcome = dispatch(instance, path);
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the ordinary managed RNG call must finish: {outcome:?}; imports: {:?}",
        recorder.calls.lock()
    );
    let response = recorder
        .responses
        .lock()
        .pop()
        .expect("the RNG handler must respond");
    assert_eq!(response.status, 200, "the RNG handler must report success");
    response.body
}

fn dispatch(instance: &mut Instance, path: &str) -> wisp_engine::DispatchOutcome {
    instance.dispatch(
        &DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "GET".into(),
                url: format!("https://randomness.test{path}"),
                path: path.into(),
                query: vec![(
                    "count".into(),
                    (wisp_abi::imports::MAX_OUT_BUFFER_BYTES + 1).to_string(),
                )],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: 30_000,
            now_ms: 1_700_000_000_000,
        },
        budget(),
    )
}

#[test]
fn static_rng_draws_fresh_host_bytes_for_each_call_request_and_template_activation() {
    for first_byte in [0x31, 0x71] {
        let (mut instance, recorder) = activate(first_byte);
        for _ in 0..2 {
            let before = recorder.entropy_draws.lock().len();
            let body = request(&mut instance, &recorder, "/draw");
            let draws = recorder.entropy_draws.lock();
            let new = &draws[before..];
            assert_eq!(
                new.len(),
                2,
                "Fill and GetBytes must each reach host entropy on every request"
            );
            assert_eq!(new[0].len(), 2, "Fill must request only its span slice");
            assert_eq!(new[1].len(), 3, "GetBytes must request its exact count");
            let expected = format!(
                "11{:02X}{:02X}44:{:02X}{:02X}{:02X}",
                new[0][0], new[0][1], new[1][0], new[1][1], new[1][2]
            );
            assert_eq!(body, expected.as_bytes(), "managed callers must observe these host draws without overwriting bytes outside the span");
        }
    }
}

#[test]
fn empty_static_rng_requests_do_not_draw_host_entropy() {
    let (mut instance, recorder) = activate(0x41);
    let before = recorder.entropy_draws.lock().len();
    assert_eq!(
        request(&mut instance, &recorder, "/empty"),
        b"0",
        "GetBytes(0) must return an empty array and Fill(empty) must succeed"
    );
    assert_eq!(
        recorder.entropy_draws.lock().len(),
        before,
        "empty random requests must not draw entropy"
    );
}

#[test]
fn a_negative_static_rng_count_reports_the_standard_argument_error_without_a_draw() {
    let (mut instance, recorder) = activate(0x51);
    let before = recorder.entropy_draws.lock().len();
    assert_eq!(
        request(&mut instance, &recorder, "/negative"),
        b"ArgumentOutOfRangeException:count:-1",
        "GetBytes must retain the standard exception type, parameter and actual value"
    );
    assert_eq!(
        recorder.entropy_draws.lock().len(),
        before,
        "an invalid count must not draw entropy"
    );
}

#[test]
fn static_rng_accepts_buffers_larger_than_one_host_transfer() {
    let (mut instance, recorder) = activate(0x61);
    let before = recorder.entropy_draws.lock().len();
    let body = request(&mut instance, &recorder, "/large");
    let draws = recorder.entropy_draws.lock();
    let new = &draws[before..];
    assert_eq!(
        new.len(),
        2,
        "a large managed draw must use two bounded host transfers"
    );
    assert_eq!(
        new[0].len(),
        wisp_abi::imports::MAX_OUT_BUFFER_BYTES,
        "the first chunk must use the canonical host cap"
    );
    assert_eq!(
        new[1].len(),
        1,
        "the second chunk must draw the remaining byte"
    );
    let expected = format!(
        "{}:{:02X}:{:02X}:1",
        wisp_abi::imports::MAX_OUT_BUFFER_BYTES + 1,
        new[0][0],
        new[1][0]
    );
    assert_eq!(
        body,
        expected.as_bytes(),
        "every byte across the chunk boundary must reflect its host draw"
    );
}

#[test]
fn a_denied_entropy_source_cannot_be_reported_as_successful_managed_randomness() {
    let (mut instance, recorder) = activate(0x81);
    recorder
        .deny_entropy
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let outcome = dispatch(&mut instance, "/draw");
    assert!(
        outcome.completion.tombstones(),
        "a failed host entropy draw must terminate the guest: {outcome:?}"
    );
    assert!(
        recorder.responses.lock().is_empty(),
        "a denied entropy source must not produce a successful managed RNG response"
    );
}

#[test]
fn a_restored_warm_managed_heap_draws_from_its_new_host_entropy_source() {
    let (mut original, original_recorder) = activate(0x21);
    request(&mut original, &original_recorder, "/draw");
    let image = original
        .capture_mutable_state()
        .expect("the completed managed RNG request must be capturable");
    let memory = original.memory().to_vec();
    drop(original);

    let (engine, template) = runtime();
    let recorder = Recorder::default();
    let mut bindings = TestBindings::new(recorder.clone());
    bindings.fill_byte = 0xC1;
    let mut restored = engine
        .activate(template, Box::new(bindings))
        .expect("the warm managed heap must activate with new host bindings");
    restored
        .restore_mutable_state(&image)
        .expect("the captured managed globals must restore");
    restored.memory().copy_from_slice(&memory);

    assert_eq!(
        request(&mut restored, &recorder, "/draw"),
        b"11C1C144:C2C2C2",
        "a restored managed runtime must use the new host source, not captured entropy"
    );
    assert_eq!(
        *recorder.entropy_draws.lock(),
        vec![vec![0xC1; 2], vec![0xC2; 3]],
        "both RNG calls after warm restoration must reach the new host exactly once"
    );
}
