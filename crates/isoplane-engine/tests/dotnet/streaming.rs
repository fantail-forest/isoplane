//! Rejected and pending host response writes are observed through the real
//! managed SDK.

use std::sync::atomic::Ordering;

use crate::dotnet::{budget, fixture, Fixture, GuestRuntime, Request};
use isoplane_abi::result::ResultRecord;
use isoplane_engine::{Completion, InstanceOps};

fn streaming(runtime: GuestRuntime) -> &'static Fixture {
    fixture("isoplane_guest_dotnet_streaming", runtime)
}

in_both_runtimes! {
    fn the_managed_sdk_rejects_a_refused_response_handle_before_exposing_a_writer(runtime: GuestRuntime) {
        let (mut instance, recorder) = streaming(runtime).activate();
        recorder.reject_next_response.store(true, Ordering::Relaxed);
        let outcome = Request::get("/refused").dispatch(&mut instance);
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "the guest must catch refused response creation: {outcome:?}"
        );
        let responses = recorder.responses.lock();
        assert_eq!(
            responses.len(),
            1,
            "only the fallback response must be accepted"
        );
        assert_eq!(
            responses[0].body, b"creation-rejected",
            "the real managed SDK must throw for the refused handle"
        );
    }

    fn a_pending_core_response_write_blocks_concurrent_writes_and_completion_until_acknowledged(runtime: GuestRuntime) {
        let (mut instance, recorder) = streaming(runtime).activate();
        let outcome = Request::get("/pending").dispatch(&mut instance);
        assert_eq!(outcome.completion, Completion::Parked, "the guest must await the first write acknowledgement after observing both guards: {outcome:?}");
        assert_eq!(
            recorder
                .calls
                .lock()
                .iter()
                .filter(|call| call.starts_with("resp_write("))
                .count(),
            1,
            "the rejected concurrent write must never enter the host"
        );
        assert!(
            !recorder
                .calls
                .lock()
                .iter()
                .any(|call| call.starts_with("resp_finish(")),
            "completion while a write is pending must not finish the host response"
        );
        let outcome = instance.wake(1, &ResultRecord::Ok(vec![1]), budget());
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "a full-write acknowledgement must resume and complete the guarded response: {outcome:?}"
        );
        let calls = recorder.calls.lock();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("resp_write("))
                .count(),
            1,
            "acknowledgement must not replay the refused write"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("resp_finish("))
                .count(),
            1,
            "the runtime must finish the response exactly once after acknowledgement"
        );
    }
}
