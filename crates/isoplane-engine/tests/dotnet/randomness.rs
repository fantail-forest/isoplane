//! Ordinary managed RNG calls use host entropy without cloned PRNG state.

use crate::bindings::Recorder;
use crate::dotnet::{budget, fixture, Fixture, GuestRuntime, Request};
use isoplane_engine::{Completion, DispatchOutcome, Instance};

fn randomness(runtime: GuestRuntime) -> &'static Fixture {
    fixture("isoplane_guest_dotnet_randomness", runtime)
}

/// An initialized instance whose response and exception paths are already
/// warm, so their own entropy is not mistaken for an explicit RNG call.
fn activate(runtime: GuestRuntime, first_byte: u8) -> (Instance, Recorder) {
    let (mut instance, recorder) = randomness(runtime).activate_with_entropy(first_byte);
    let initialized = instance.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the managed RNG fixture must initialize: {initialized:?}"
    );
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
    assert_eq!(
        request(&mut instance, &recorder, "/instance-null"),
        b"ArgumentNullException:data",
        "instance RNG validation must warm without drawing bytes"
    );
    (instance, recorder)
}

fn probe(path: &str) -> Request {
    Request::get(path)
        .query("count", isoplane_abi::imports::MAX_OUT_BUFFER_BYTES + 1)
        .now_ms(1_700_000_000_000)
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    probe(path).answer(instance, recorder)
}

fn dispatch(instance: &mut Instance, path: &str) -> DispatchOutcome {
    probe(path).dispatch(instance)
}

in_mono_runtime! {
    fn ordinary_rng_draws_fresh_host_bytes_for_each_call_request_and_template_activation(runtime: GuestRuntime) {
        for path in ["/draw", "/instance"] {
            let mut bodies = Vec::new();
            for first_byte in [0x31, 0x71] {
                let (mut instance, recorder) = activate(runtime, first_byte);
                for _ in 0..2 {
                    let before = recorder.entropy_draws.lock().len();
                    let body = request(&mut instance, &recorder, path);
                    let draws = recorder.entropy_draws.lock();
                    let new = &draws[before..];
                    // The DRBG reseeds from the host on every request
                    // (prediction resistance) and answers with its own
                    // output; the span discipline holds.
                    assert!(
                        !new.is_empty(),
                        "every managed request must reseed from host entropy"
                    );
                    let text = String::from_utf8_lossy(&body).to_string();
                    assert!(
                        text.len() == 15 && text.starts_with("11") && text[6..9] == *"44:",
                        "Fill must write only its two-byte slice and GetBytes its three bytes: {text}"
                    );
                    bodies.push(text);
                }
            }
            let distinct: std::collections::BTreeSet<_> = bodies.iter().collect();
            assert_eq!(
                distinct.len(),
                bodies.len(),
                "every request in every activation must draw distinct bytes: {bodies:?}"
            );
        }
    }

    fn empty_rng_requests_do_not_draw_host_entropy(runtime: GuestRuntime) {
        for path in ["/empty", "/instance-empty"] {
            let (mut instance, recorder) = activate(runtime, 0x41);
            let before = recorder.entropy_draws.lock().len();
            assert_eq!(
                request(&mut instance, &recorder, path),
                b"0",
                "GetBytes(0) must return an empty array and Fill(empty) must succeed"
            );
            assert_eq!(
                recorder.entropy_draws.lock().len(),
                before,
                "empty random requests must not draw entropy"
            );
        }
    }

    fn a_negative_static_rng_count_reports_the_standard_argument_error_without_a_draw(runtime: GuestRuntime) {
        let (mut instance, recorder) = activate(runtime, 0x51);
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

    fn ordinary_rng_accepts_buffers_larger_than_one_host_transfer(runtime: GuestRuntime) {
        for path in ["/large", "/instance-large"] {
            let (mut instance, recorder) = activate(runtime, 0x61);
            let before = recorder.entropy_draws.lock().len();
            let body = request(&mut instance, &recorder, path);
            let draws = recorder.entropy_draws.lock();
            let new = &draws[before..];
            // The DRBG fills the whole buffer from a host-seeded state.
            assert!(!new.is_empty(), "a large managed draw must reseed from the host");
            let text = String::from_utf8_lossy(&body).to_string();
            assert!(
                text.starts_with(&format!("{}:", isoplane_abi::imports::MAX_OUT_BUFFER_BYTES + 1)),
                "the whole buffer must be filled: {text}"
            );
        }
    }

    fn a_denied_entropy_source_cannot_be_reported_as_successful_managed_randomness(runtime: GuestRuntime) {
        for path in ["/draw", "/instance"] {
            let (mut instance, recorder) = activate(runtime, 0x81);
            recorder
                .deny_entropy
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let outcome = dispatch(&mut instance, path);
            assert!(
                outcome.completion.tombstones(),
                "a failed host entropy draw must terminate the guest: {outcome:?}"
            );
            assert!(
                recorder.responses.lock().is_empty(),
                "a denied entropy source must not produce a successful managed RNG response"
            );
        }
    }

    fn a_restored_warm_managed_heap_draws_from_its_new_host_entropy_source(runtime: GuestRuntime) {
        for path in ["/draw", "/instance"] {
            let (mut original, original_recorder) = activate(runtime, 0x21);
            let first = request(&mut original, &original_recorder, path);
            let (mut restored, recorder) = randomness(runtime).restore_from(original, 0xC1);
            let after = request(&mut restored, &recorder, path);
            assert!(
                !recorder.entropy_draws.lock().is_empty(),
                "the DRBG in a restored heap must reseed from the new host"
            );
            assert_ne!(
                after, first,
                "a restored managed runtime must not replay the captured generator state"
            );
        }
    }

    fn managed_crypto_value_types_preserve_their_ordinary_state(runtime: GuestRuntime) {
        let (mut instance, recorder) = activate(runtime, 0x31);
        assert_eq!(
            request(&mut instance, &recorder, "/values"),
            b"values",
            "ordinary crypto value and collection types must retain their managed state"
        );
    }

    fn a_null_instance_rng_buffer_reports_the_standard_error_without_a_draw(runtime: GuestRuntime) {
        let (mut instance, recorder) = activate(runtime, 0x31);
        let before = recorder.entropy_draws.lock().len();
        assert_eq!(
            request(&mut instance, &recorder, "/instance-null"),
            b"ArgumentNullException:data",
            "null instance buffers must retain the standard exception parameter"
        );
        assert_eq!(
            recorder.entropy_draws.lock().len(),
            before,
            "invalid instance buffers must not consume entropy"
        );
    }

    fn invalid_instance_rng_ranges_report_standard_parameter_names_without_draws(runtime: GuestRuntime) {
        let (mut instance, recorder) = activate(runtime, 0x31);
        for (path, expected) in [
            ("/instance-offset", "ArgumentOutOfRangeException:offset"),
            ("/instance-count", "ArgumentOutOfRangeException:count"),
        ] {
            request(&mut instance, &recorder, path);
            let before = recorder.entropy_draws.lock().len();
            assert_eq!(
                request(&mut instance, &recorder, path),
                expected.as_bytes(),
                "invalid instance RNG ranges must retain ordinary parameter names"
            );
            assert_eq!(
                recorder.entropy_draws.lock().len(),
                before,
                "invalid instance ranges must not draw host entropy"
            );
        }
    }
}
