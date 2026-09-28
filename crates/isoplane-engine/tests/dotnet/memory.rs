//! The real collectors observe host memory capacity before the template is captured.
use crate::dotnet::{budget, Fixture, GuestRuntime, Request};
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};

fn initialized_limit_survives_restore(wasm: &[u8], limit_bytes: u64) {
    let engine = IsoplaneEngine::new(EngineConfig {
        max_instances: 2,
        pooling: false,
        max_memory_bytes: limit_bytes,
        guard_size_bytes: 2 << 20,
        ..EngineConfig::default()
    })
    .expect("the managed memory probe engine must start");
    let template = engine
        .load_template(wasm)
        .expect("the managed memory probe must load");
    let fixture = Fixture { engine, template };
    let (mut original, _) = fixture.activate();
    let initialized = original.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the managed memory probe must initialize under the effective ceiling: {initialized:?}"
    );
    let (mut restored, recorder) = fixture.restore_from(original, 0xAB);
    let response = String::from_utf8(Request::get("/").answer(&mut restored, &recorder))
        .expect("the memory probe must return decimal byte counts");
    assert_eq!(response, format!("{limit_bytes}:{limit_bytes}"),
        "the collector must observe the effective limit at initialization and retain it after capture and restoration");
}

in_mono_runtime! {
    fn the_gc_observes_its_limit_during_initialization_and_after_restore(runtime: GuestRuntime) {
        let wasm = isoplane_fixtures::dotnet_for("isoplane_guest_dotnet_memory", runtime);
        for limit_bytes in [96 << 20, isoplane_abi::config::WASM32_MEMORY_MAX_BYTES] {
            initialized_limit_survives_restore(&wasm, limit_bytes);
        }
    }
}
