//! The real collectors observe host memory capacity before the template is captured.
use crate::dotnet::{budget, fixture, Fixture, GuestRuntime, Request};
use isoplane_engine::Completion;

fn initialized_limit_survives_restore(runtime: GuestRuntime, limit_bytes: u64) {
    let fixture = Fixture {
        memory_limit_bytes: Some(limit_bytes),
        ..fixture("isoplane_guest_dotnet_bcl", runtime).clone()
    };
    let (mut original, _) = fixture.activate();
    let initialized = original.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the managed memory probe must initialize under the effective ceiling: {initialized:?}"
    );
    let (mut restored, recorder) = fixture.restore_from(original, 0xAB);
    let response = String::from_utf8(Request::get("/memory").answer(&mut restored, &recorder))
        .expect("the memory probe must return decimal byte counts");
    assert_eq!(response, format!("{limit_bytes}:{limit_bytes}"),
        "the collector must observe the effective limit at initialization and retain it after capture and restoration");
}

in_mono_runtime! {
    fn the_gc_observes_its_limit_during_initialization_and_after_restore(runtime: GuestRuntime) {
        for limit_bytes in [96 << 20, 128 << 20] {
            initialized_limit_survives_restore(runtime, limit_bytes);
        }
    }
}
