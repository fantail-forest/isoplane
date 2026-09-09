//! Standard managed SHA-256 and HMAC-SHA-256 APIs through a real WASM guest.

#[path = "common/bindings.rs"]
mod bindings;

use bindings::{Recorder, TestBindings};
use std::sync::OnceLock;
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_engine::{
    Completion, CpuBudget, EngineConfig, Instance, InstanceOps, Template, WispEngine,
};

fn activate() -> (Instance, Recorder) {
    static RUNTIME: OnceLock<(WispEngine, Template)> = OnceLock::new();
    let (engine, template) = RUNTIME.get_or_init(|| {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 8,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the hash fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_hashing"))
            .expect("the hash fixture must load");
        (engine, template)
    });
    let recorder = Recorder::default();
    let instance = engine
        .activate(template, Box::new(TestBindings::new(recorder.clone())))
        .expect("the hash fixture must activate");
    (instance, recorder)
}

fn hash(
    instance: &mut Instance,
    recorder: &Recorder,
    path: &str,
    key: &str,
    source: &[u8],
) -> Vec<u8> {
    let outcome = instance.dispatch(
        &DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "POST".into(),
                url: format!("https://hash.test{path}"),
                path: path.into(),
                query: vec![("key".into(), key.into())],
                headers: vec![],
                body: source.to_vec(),
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
        "the ordinary managed hash call must finish: {outcome:?}"
    );
    let response = recorder
        .responses
        .lock()
        .pop()
        .expect("the hash handler must respond");
    assert_eq!(response.status, 200, "the hash handler must report success");
    response.body
}

#[test]
fn ordinary_sha256_array_and_span_calls_match_known_digests_across_requests() {
    let (mut instance, recorder) = activate();
    let cases: &[(&[u8], &str)] = &[
        (
            b"",
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
        ),
        (
            b"abc",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248D6A61D20638B8E5C026930C3E6039A33CE45964FF2167F6ECEDD419DB06C1",
        ),
    ];
    for path in ["/sha-array", "/sha-span"] {
        for (source, expected) in cases {
            assert_eq!(
                hash(&mut instance, &recorder, path, "", source),
                expected.as_bytes(),
                "SHA-256 must match the known digest and retain no previous request input"
            );
        }
    }
}

#[test]
fn ordinary_hmac_sha256_matches_rfc4231_short_and_long_key_vectors() {
    let (mut instance, recorder) = activate();
    let cases = [
        (
            "0b".repeat(20),
            b"Hi There".as_slice(),
            "B0344C61D8DB38535CA8AFCEAF0BF12B881DC200C9833DA726E9376C2E32CFF7",
        ),
        (
            "aa".repeat(131),
            b"Test Using Larger Than Block-Size Key - Hash Key First".as_slice(),
            "60E431591EE0B67F0D8A26AACBF5B77F8E0BC6213728C5140546040F0EE37F54",
        ),
        (
            String::new(),
            b"".as_slice(),
            "B613679A0814D9EC772F95D778C35FC5FF1697C493715653C6C712144292C5AD",
        ),
    ];
    for path in ["/hmac-array", "/hmac-span"] {
        for (key, source, expected) in &cases {
            assert_eq!(hash(&mut instance, &recorder, path, key, source), expected.as_bytes(),
                "HMAC-SHA-256 must authenticate with the current key, including empty and over-block-size keys");
        }
    }
}

#[test]
fn managed_sha256_hashes_binary_inputs_larger_than_a_host_transfer() {
    let (mut instance, recorder) = activate();
    let source = vec![0xa5; 1024 * 1024 + 1];
    for path in ["/sha-array", "/sha-span"] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "", &source),
            b"9311676D75BE2345E88872475314A1E57EB737858A3D58D76B8E1191166DCC1C",
            "managed SHA-256 must hash every byte without a host transfer cap"
        );
    }
}

#[test]
fn managed_hash_array_overloads_reject_null_arguments_by_name() {
    let (mut instance, recorder) = activate();
    for (path, parameter) in [
        ("/sha-null", "source"),
        ("/hmac-null-key", "key"),
        ("/hmac-null-source", "source"),
    ] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "", b"input"),
            format!("ArgumentNullException:{parameter}").as_bytes(),
            "hash overloads must retain the standard null argument contract"
        );
    }
}
