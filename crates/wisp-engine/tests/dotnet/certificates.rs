//! Ordinary certificate value operations.

use crate::bindings::Recorder;
use crate::dotnet::{fixture, Fixture, GuestRuntime, Request};
use wisp_engine::Instance;

fn certificates(runtime: GuestRuntime) -> &'static Fixture {
    fixture("wisp_guest_dotnet_certificates", runtime)
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    Request::post(path).answer(instance, recorder)
}

in_both_runtimes! {
    fn ordinary_certificate_clones_preserve_owned_der_and_signature_hash_values(runtime: GuestRuntime) {
        let (mut instance, recorder) = certificates(runtime).activate();
        for path in ["/values", "/legacy-values"] {
            assert_eq!(request(&mut instance, &recorder, path),
                b"1.2.840.113549.1.1.11:sha256RSA:7B96FADDA73D83CFB7A489C8959232CE980CAF3894C7FB2E7E4AE983D7232E2F",
                "certificate clones must retain exact DER, signature metadata and the channel-binding digest after original disposal");
        }
    }

    fn certificate_loading_refuses_malformed_and_empty_der(runtime: GuestRuntime) {
        let (mut instance, recorder) = certificates(runtime).activate();
        for path in ["/malformed", "/empty"] {
            assert_eq!(
                String::from_utf8_lossy(&request(&mut instance, &recorder, path)),
                "CryptographicException",
                "certificate loading must refuse DER that holds no certificate ({path})"
            );
        }
        // One certificate is read and what follows it is ignored, as .NET
        // does on Linux.
        let trailing = String::from_utf8_lossy(&request(&mut instance, &recorder, "/trailing")).to_string();
        assert!(
            trailing.starts_with("30820351"),
            "the certificate ahead of a trailing byte must load: {trailing}"
        );
    }

    fn certificate_operations_beyond_the_der_value_are_available(runtime: GuestRuntime) {
        let (mut instance, recorder) = certificates(runtime).activate();
        let answer = |instance: &mut Instance, path: &str| {
            String::from_utf8_lossy(&request(instance, &recorder, path)).to_string()
        };
        // A foreign native handle is undefined in .NET itself and is not probed.
        let handle: u64 = answer(&mut instance, "/handle")
            .parse()
            .expect("a certificate's native handle is an address");
        assert_ne!(handle, 0, "a certificate must expose a live native handle");
        assert_eq!(
            answer(&mut instance, "/subject"),
            "CN=tls-probe.test",
            "the subject must be read from the certificate"
        );
        assert!(
            answer(&mut instance, "/flags").starts_with("30820351"),
            "a certificate must load with ephemeral key storage flags"
        );
    }

    fn a_disposed_certificate_refuses_value_access(runtime: GuestRuntime) {
        let (mut instance, recorder) = certificates(runtime).activate();
        assert_eq!(
            request(&mut instance, &recorder, "/disposed"),
            b"CryptographicException",
            "a disposed certificate must not hand out its values"
        );
    }

    fn retained_ordinary_certificate_values_survive_capture_and_restore(runtime: GuestRuntime) {
        let fixture = certificates(runtime);
        let (mut original, recorder) = fixture.activate();
        assert_eq!(
            request(&mut original, &recorder, "/capture"),
            b"saved",
            "the original guest must retain its ordinary certificate before capture"
        );
        let (mut restored, recorder) = fixture.restore_from(original, 0xAB);
        assert_eq!(request(&mut restored, &recorder, "/restored"),
            b"1.2.840.113549.1.1.11:sha256RSA:7B96FADDA73D83CFB7A489C8959232CE980CAF3894C7FB2E7E4AE983D7232E2F",
            "restored certificate must preserve exact DER, signature metadata and digest after the original instance is destroyed");
    }
}
