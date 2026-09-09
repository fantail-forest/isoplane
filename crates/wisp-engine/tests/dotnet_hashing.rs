//! Standard managed SHA-256 and HMAC-SHA-256 APIs through a real WASM guest.

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
        .expect("the hash fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet("wisp_guest_dotnet_hashing"))
            .expect("the hash fixture must load");
        (engine, template)
    })
}

fn activate() -> (Instance, Recorder) {
    let (engine, template) = runtime();
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
    for path in [
        "/sha-array",
        "/sha-span",
        "/sha-instance",
        "/sha-reset",
        "/sha-short-output",
    ] {
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
    for path in ["/hmac-array", "/hmac-span", "/hmac-instance", "/hmac-reset"] {
        for (key, source, expected) in &cases {
            assert_eq!(
                hash(&mut instance, &recorder, path, key, source),
                expected.as_bytes(),
                "HMAC-SHA-256 must authenticate with the current key, including empty and over-block-size keys"
            );
        }
    }
}

#[test]
fn managed_sha256_hashes_binary_inputs_larger_than_a_host_transfer() {
    let (mut instance, recorder) = activate();
    let source = vec![0xa5; 1024 * 1024 + 1];
    for path in [
        "/sha-array",
        "/sha-span",
        "/sha-instance",
        "/sha-reset",
        "/sha-short-output",
    ] {
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

#[test]
fn managed_pbkdf2_preserves_byte_string_and_span_vectors() {
    let (mut instance, recorder) = activate();
    let cases: &[(&[u8], &str, i32, usize, &str)] = &[
        (
            &[112, 97, 115, 115, 119, 111, 114, 100],
            "73616c74",
            1,
            32,
            "120FB6CFFCF8B32C43E7225256C4F837A86548C92CCC35480805987CB70BE17B",
        ),
        (
            &[112, 97, 115, 115, 119, 111, 114, 100],
            "73616c74",
            2,
            32,
            "AE4D0C95AF6B46D32D0ADFF928F06DD02A303F8EF3C251DFD6E2D85A95474C43",
        ),
        (
            &[112, 97, 115, 115, 119, 111, 114, 100],
            "73616c74",
            4096,
            32,
            "C5E478D59288C841AA530DB6845C4C8D962893A001CE4E11A4963873AA98134A",
        ),
        (
            &[112, 195, 164, 115, 115, 240, 159, 148, 145],
            "73616c74",
            2,
            65,
            "D78329FDFF1B8077B9097D93C6580323FFA3061182F57C302DC88EDAA80AB2E3FD4D49F909B57CE5F6656966A4C903AC93C119C348C66E822E40EB88D7333A071A",
        ),
        (
            &[112, 97, 115, 115, 0, 119, 111, 114, 100],
            "7361006c74",
            2,
            65,
            "AA4399833B716BE66298125C3E643697F19AC0A893C1A93EF0665590C0CDE81678ED9F5030C7CFFD74FC0E06CD1C2F570352D59939EFF13EFE47D45984A85BBAFB",
        ),
        (
            &[
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112,
                112, 112, 112, 112,
            ],
            "73616c74",
            2,
            65,
            "7FB39A0C2291DE62231E50AB5F6805B83BAB97446D73DCCF38114FB21C05542759977CA37B50559F49BF273AED0F0256326784BEE144FE079373E38DCCC10741F1",
        ),
    ];
    for kind in ["bytes", "string", "span"] {
        for (password, salt, iterations, count, expected) in cases {
            let path = format!("/pbkdf2/{kind}/{iterations}/{count}");
            assert_eq!(
                hash(&mut instance, &recorder, &path, salt, password),
                expected.as_bytes(),
                "PBKDF2 must preserve encoding, embedded zeros and multi-block derivation"
            );
        }
        assert!(
            hash(
                &mut instance,
                &recorder,
                &format!("/pbkdf2/{kind}/1/0"),
                "",
                b""
            )
            .is_empty(),
            "zero-length derivation must return no bytes"
        );
    }
}

#[test]
fn managed_pbkdf2_retains_standard_argument_guards() {
    let (mut instance, recorder) = activate();
    for (path, expected) in [
        ("/pbkdf-null-password", "ArgumentNullException:password"),
        ("/pbkdf-null-salt", "ArgumentNullException:salt"),
        (
            "/pbkdf2/bytes/0/32",
            "ArgumentOutOfRangeException:iterations",
        ),
        (
            "/pbkdf2/string/-1/32",
            "ArgumentOutOfRangeException:iterations",
        ),
        (
            "/pbkdf2/bytes/1/-1",
            "ArgumentOutOfRangeException:outputLength",
        ),
    ] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "", b"password"),
            expected.as_bytes(),
            "PBKDF2 must retain the ordinary parameter error"
        );
    }
}

#[test]
fn foreign_hmac_operations_fail_explicitly_and_disposal_prevents_reuse() {
    let (mut instance, recorder) = activate();
    for path in ["/hmac-clone", "/hmac-current", "/hmac-sha1", "/pbkdf-sha1"] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "6b6579", b""),
            b"PlatformNotSupportedException",
            "unsupported foreign HMAC operations must fail explicitly"
        );
    }
    for path in ["/hmac-disposed", "/hmac-provider-disposed"] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "6b6579", b""),
            b"ObjectDisposedException",
            "disposed keyed state must refuse further use"
        );
    }
}

#[test]
fn live_managed_hmac_state_survives_capture_and_restore() {
    let (mut original, recorder) = activate();
    assert!(
        hash(&mut original, &recorder, "/hmac-start", "6b6579", b"a").is_empty(),
        "starting a digest must leave the keyed state unfinished"
    );
    let image = original
        .capture_mutable_state()
        .expect("live managed HMAC state must be capturable");
    let memory = original.memory().to_vec();
    drop(original);
    let (mut restored, recorder) = activate();
    restored
        .restore_mutable_state(&image)
        .expect("captured keyed globals must restore");
    restored.memory().copy_from_slice(&memory);
    assert_eq!(
        hash(&mut restored, &recorder, "/hmac-finish", "", b"bc"),
        b"9C196E32DC0175F86F4B1CB89289D6619DE6BEE699E4C378E68309ED97A1A6AB",
        "restored incremental HMAC must retain the key and pre-capture prefix"
    );
}

#[test]
fn ordinary_sha256_preserves_argument_disposal_and_algorithm_guards() {
    let (mut instance, recorder) = activate();
    for (path, expected) in [
        ("/sha-instance-null", "ArgumentNullException:buffer"),
        ("/sha-instance-offset", "ArgumentOutOfRangeException:offset"),
        ("/sha-instance-disposed", "ObjectDisposedException"),
        ("/sha-unsupported", "PlatformNotSupportedException"),
        ("/sha-clone", "PlatformNotSupportedException"),
        ("/sha-current", "PlatformNotSupportedException"),
        ("/sha-provider-disposed", "ObjectDisposedException"),
    ] {
        assert_eq!(
            hash(&mut instance, &recorder, path, "", b"abc"),
            expected.as_bytes(),
            "ordinary hash instances must preserve argument and lifetime guards"
        );
    }
}

#[test]
fn unfinished_ordinary_sha256_state_survives_capture_and_restore() {
    let (mut original, recorder) = activate();
    assert!(
        hash(&mut original, &recorder, "/sha-start", "", b"a").is_empty(),
        "starting SHA256 must leave the digest unfinished"
    );
    let image = original
        .capture_mutable_state()
        .expect("managed digest state must capture");
    let memory = original.memory().to_vec();
    drop(original);
    let (mut restored, recorder) = activate();
    restored
        .restore_mutable_state(&image)
        .expect("digest globals must restore");
    restored.memory().copy_from_slice(&memory);
    assert_eq!(
        hash(&mut restored, &recorder, "/sha-finish", "", b"bc"),
        b"BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
        "restored SHA256 must retain the unfinished pre-capture prefix"
    );
}
