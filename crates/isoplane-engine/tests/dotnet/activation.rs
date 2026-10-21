//! Activation: generators seeded before the template snapshot are reseeded
//! when an instance activates, so two activations of the template draw apart.
//! The runtime reseeds every generator that chose its own seed; an app
//! recreates the ones it seeded itself in the SDK's activation hook. Request
//! identifiers, which ASP.NET numbers from a counter the snapshot copies,
//! must likewise differ between activations.

use crate::bindings::Recorder;
use crate::dotnet::{budget, fixture, Fixture, GuestRuntime, Request};
use isoplane_engine::{Completion, Instance, InstanceOps};

fn activation(runtime: GuestRuntime) -> &'static Fixture {
    fixture("isoplane_guest_dotnet_activation", runtime)
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    Request::get(path)
        .now_ms(1_700_000_000_000)
        .answer(instance, recorder)
}

/// Two activations of one template: the capture is taken after
/// pre-initialisation and before any dispatch, as the template is, so the
/// generator the probe seeded at boot is in both copies.
fn cloned_instances(runtime: GuestRuntime) -> Vec<(Instance, Recorder)> {
    let fixture = activation(runtime);
    let (mut original, recorder) = fixture.activate_with_entropy(0x21);
    *recorder.runtime_configuration.lock() = Some(br#"{"probe":"captured"}"#.to_vec());
    let initialized = original.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the activation fixture must initialize"
    );
    assert!(
        !recorder.entropy_draws.lock().is_empty(),
        "the probe must seed its generator during pre-initialisation, so the template holds it"
    );
    clone_twice(fixture, original)
}

/// Two activations of the ASP.NET fixture's template, captured after the
/// warm-up requests its initialization makes.
fn cloned_aspnet_instances(runtime: GuestRuntime) -> Vec<(Instance, Recorder)> {
    let fixture = fixture("isoplane_guest_aspnet", runtime);
    let (mut original, _) = fixture.activate_with_entropy(0x21);
    let initialized = original.run_initialize(budget());
    assert_eq!(
        initialized.completion,
        Completion::Done,
        "the ASP.NET fixture must initialize"
    );
    clone_twice(fixture, original)
}

fn clone_twice(fixture: &Fixture, mut original: Instance) -> Vec<(Instance, Recorder)> {
    let state = original
        .capture_mutable_state()
        .expect("the warm heap must be capturable");
    let memory = original.memory().to_vec();
    drop(original);
    [0x31, 0x71]
        .into_iter()
        .map(|first_byte| {
            let (mut instance, recorder) = fixture.activate_with_entropy(first_byte);
            instance
                .restore_mutable_state(&state)
                .expect("both activations must restore the same captured managed state");
            instance.memory().copy_from_slice(&memory);
            (instance, recorder)
        })
        .collect()
}

/// The identifier ASP.NET gave a request, which the fixture echoes in a
/// header. The host acknowledges each write of an ASP.NET response.
fn request_identifier(instance: &mut Instance, recorder: &Recorder) -> String {
    let mut outcome = Request::get("/request-id").dispatch(instance);
    for _ in 0..8 {
        if outcome.completion != Completion::Parked {
            break;
        }
        let (future, result) = recorder
            .take_ready(1)
            .expect("a parked ASP.NET response must request a slice or await a write");
        outcome = instance.wake(1, future, &result, budget());
    }
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the ASP.NET fixture must answer /request-id: {outcome:?}"
    );
    let response = recorder
        .responses
        .lock()
        .pop()
        .expect("the ASP.NET fixture must respond to /request-id");
    response
        .headers
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-request-id"))
        .map(|(_, value)| value)
        .expect("the fixture must echo the request identifier")
}

/// One draw from each of two activations of one template.
fn draws(runtime: GuestRuntime, path: &str) -> Vec<Vec<u8>> {
    cloned_instances(runtime)
        .into_iter()
        .map(|(mut instance, recorder)| request(&mut instance, &recorder, path))
        .collect()
}

fn assert_activations_draw_apart(runtime: GuestRuntime, path: &str, generator: &str) {
    let draws = draws(runtime, path);
    assert_ne!(
        draws[0], draws[1],
        "two activations of one template must not replay the {generator} state the snapshot holds"
    );
}

in_mono_runtime! {
    fn the_runtimes_shared_generator_is_reseeded_in_each_activation(runtime: GuestRuntime) {
        assert_activations_draw_apart(runtime, "/shared-draw", "Random.Shared");
    }

    fn the_runtimes_trace_id_generator_is_reseeded_in_each_activation(runtime: GuestRuntime) {
        assert_activations_draw_apart(runtime, "/trace-id", "trace identifier");
    }

    fn a_generator_an_app_created_without_a_seed_is_reseeded_in_each_activation(runtime: GuestRuntime) {
        assert_activations_draw_apart(runtime, "/unseeded-draw", "unseeded Random");
        assert_activations_draw_apart(runtime, "/unseeded-derived-draw", "unseeded derived Random");
    }

    fn aspnet_request_identifiers_differ_between_activations(runtime: GuestRuntime) {
        let identifiers: Vec<_> = cloned_aspnet_instances(runtime)
            .into_iter()
            .map(|(mut instance, recorder)| request_identifier(&mut instance, &recorder))
            .collect();
        assert_ne!(
            identifiers[0], identifiers[1],
            "two activations of one template must not number their requests alike"
        );
    }

    fn a_generator_given_a_fixed_seed_keeps_its_sequence_in_every_activation(runtime: GuestRuntime) {
        let draws = draws(runtime, "/fixed-seed-draw");
        assert_eq!(
            draws[0], draws[1],
            "a generator seeded by the app is deterministic by request and must not be reseeded"
        );
    }

    fn initialization_can_capture_settings_before_activation(runtime: GuestRuntime) {
        for (mut instance, recorder) in cloned_instances(runtime) {
            assert_eq!(request(&mut instance, &recorder, "/prepared-setting"), b"captured",
                "the SDK must allow initialization-time settings and retain them in each clone");
        }
    }

    fn initialization_hook_state_is_captured_before_instances_are_cloned(runtime: GuestRuntime) {
        for (mut instance, recorder) in cloned_instances(runtime) {
            assert_eq!(
                request(&mut instance, &recorder, "/initializations"),
                b"1",
                "each clone must inherit the completed initialization hook"
            );
            assert_eq!(
                request(&mut instance, &recorder, "/initializations"),
                b"1",
                "request handling must not rerun template initialization"
            );
        }
    }

    fn a_generator_seeded_before_the_snapshot_is_reseeded_from_each_activations_host(runtime: GuestRuntime) {
        let mut draws = Vec::new();
        for (mut instance, recorder) in cloned_instances(runtime) {
            let before = recorder.entropy_draws.lock().len();
            let draw = request(&mut instance, &recorder, "/draw");
            assert!(
                recorder.entropy_draws.lock().len() > before,
                "the activation hook must reseed from this activation's host"
            );
            draws.push(draw);
        }
        assert_ne!(
            draws[0], draws[1],
            "two activations of one template must not replay the generator the snapshot holds"
        );
    }

    fn the_hook_fires_once_per_activation_and_not_on_a_later_dispatch(runtime: GuestRuntime) {
        let (mut instance, recorder) = cloned_instances(runtime).remove(0);
        assert_eq!(
            request(&mut instance, &recorder, "/activations"),
            b"1",
            "the first dispatch of an activation raises the hook once"
        );
        assert_eq!(
            request(&mut instance, &recorder, "/activations"),
            b"1",
            "a later dispatch of the same activation must not raise it again"
        );
    }
}
