//! Ordinary certificate value operations through a real WASM guest.

#[path = "common/bindings.rs"]
mod bindings;

use bindings::{Recorder, TestBindings};
use std::sync::OnceLock;
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
        .expect("the certificate fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_certificates"))
            .expect("the certificate fixture must load");
        (engine, template)
    })
}

fn activate() -> (Instance, Recorder) {
    let (engine, template) = runtime();
    let recorder = Recorder::default();
    let instance = engine
        .activate(template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the certificate fixture must activate");
    (instance, recorder)
}

fn request(instance: &mut Instance, recorder: &Recorder, path: &str) -> Vec<u8> {
    let outcome = instance.dispatch(
        &DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "POST".into(),
                url: format!("https://certificate.test{path}"),
                path: path.into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: 30_000,
            now_ms: 1,
        },
        CpuBudget {
            cpu_ms: 30_000,
            wall_ms_cap: 60_000,
            spent_ns: 0,
        },
    );
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the ordinary managed certificate call must finish: {outcome:?}"
    );
    let response = recorder
        .responses
        .lock()
        .pop()
        .expect("the certificate handler must respond");
    assert_eq!(
        response.status, 200,
        "the certificate handler must report success"
    );
    response.body
}

#[test]
fn ordinary_certificate_clones_preserve_owned_der_and_signature_hash_values() {
    let (mut instance, recorder) = activate();
    for path in ["/values", "/legacy-values"] {
        assert_eq!(request(&mut instance, &recorder, path),
            b"1.2.840.113549.1.1.11:sha256RSA:7B96FADDA73D83CFB7A489C8959232CE980CAF3894C7FB2E7E4AE983D7232E2F",
            "certificate clones must retain exact DER, signature metadata and the channel-binding digest after original disposal");
    }
}

#[test]
fn certificate_loading_refuses_malformed_empty_and_trailing_der() {
    let (mut instance, recorder) = activate();
    for path in ["/malformed", "/empty", "/trailing"] {
        assert_eq!(
            request(&mut instance, &recorder, path),
            b"CryptographicException",
            "certificate loading must accept exactly one valid DER certificate"
        );
    }
}

#[test]
fn unavailable_certificate_operations_are_explicitly_named() {
    let (mut instance, recorder) = activate();
    for (path, expected) in [
        ("/handle", "Certificate PAL operation unsupported: System.IntPtr System.Security.Cryptography.X509Certificates.ICertificatePalCore::get_Handle()"),
        ("/from-handle", "Certificate native handle import is unsupported."),
        ("/subject", "Certificate PAL operation unsupported: System.String System.Security.Cryptography.X509Certificates.ICertificatePalCore::get_Subject()"),
        ("/flags", "Certificate key storage flags are unsupported by the DER value adapter."),
    ] {
        assert_eq!(request(&mut instance, &recorder, path), expected.as_bytes(),
            "unsupported certificate operations must identify the unavailable operation without fabricated values");
    }
}

#[test]
fn certificate_disposal_and_null_input_prevent_value_access() {
    let (mut instance, recorder) = activate();
    for (path, expected) in [
        ("/disposed", "CryptographicException"),
        ("/provider-disposed", "ObjectDisposedException"),
        ("/provider-disposed-raw", "ObjectDisposedException"),
        ("/provider-null", "ArgumentNullException:input"),
    ] {
        assert_eq!(
            request(&mut instance, &recorder, path),
            expected.as_bytes(),
            "certificate value ownership must reject disposed or missing input"
        );
    }
}

#[test]
fn retained_ordinary_certificate_values_survive_capture_and_restore() {
    let (mut original, recorder) = activate();
    assert_eq!(
        request(&mut original, &recorder, "/capture"),
        b"saved",
        "the original guest must retain its ordinary certificate before capture"
    );
    let image = original
        .capture_mutable_state()
        .expect("managed certificate state must capture");
    let memory = original.memory().to_vec();
    drop(original);
    let (mut restored, recorder) = activate();
    restored
        .restore_mutable_state(&image)
        .expect("retained certificate globals must restore");
    restored.memory().copy_from_slice(&memory);
    assert_eq!(request(&mut restored, &recorder, "/restored"),
        b"1.2.840.113549.1.1.11:sha256RSA:7B96FADDA73D83CFB7A489C8959232CE980CAF3894C7FB2E7E4AE983D7232E2F",
        "restored certificate must preserve exact DER, signature metadata and digest after the original instance is destroyed");
}
