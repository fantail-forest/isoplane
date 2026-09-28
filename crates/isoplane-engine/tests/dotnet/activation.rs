//! The SDK's activation hook: a generator an app seeded before the template
//! snapshot is recreated on activation, so two activations of the template
//! draw apart and each recreation draws host entropy.

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

in_mono_runtime! {
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
