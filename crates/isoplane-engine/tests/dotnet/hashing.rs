//! Standard managed SHA-256, HMAC-SHA-256 and PBKDF2 APIs.

use crate::bindings::Recorder;
use crate::dotnet::{fixture, Fixture, GuestRuntime, Request};
use isoplane_engine::Instance;

fn hashing(runtime: GuestRuntime) -> &'static Fixture {
    fixture("isoplane_guest_dotnet_hashing", runtime)
}

fn hash(
    instance: &mut Instance,
    recorder: &Recorder,
    path: &str,
    key: &str,
    source: &[u8],
) -> Vec<u8> {
    Request::post(path)
        .query("key", key)
        .body(source)
        .answer(instance, recorder)
}

in_both_runtimes! {
    fn ordinary_sha256_array_and_span_calls_match_known_digests_across_requests(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
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

    fn ordinary_hmac_sha256_matches_rfc4231_short_and_long_key_vectors(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
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

    fn managed_sha256_hashes_binary_inputs_larger_than_a_host_transfer(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
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

    fn managed_hash_array_overloads_reject_null_arguments_by_name(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
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

    fn managed_pbkdf2_preserves_byte_string_and_span_vectors(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
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
                &[112; 100],
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
                hash(&mut instance, &recorder, &format!("/pbkdf2/{kind}/1/0"), "", b"").is_empty(),
                "zero-length derivation must return no bytes"
            );
        }
    }

    fn managed_pbkdf2_retains_standard_argument_guards(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
        for (path, expected) in [
            ("/pbkdf-null-password", "ArgumentNullException:password"),
            ("/pbkdf-null-salt", "ArgumentNullException:salt"),
            ("/pbkdf2/bytes/0/32", "ArgumentOutOfRangeException:iterations"),
            ("/pbkdf2/string/-1/32", "ArgumentOutOfRangeException:iterations"),
            ("/pbkdf2/bytes/1/-1", "ArgumentOutOfRangeException:outputLength"),
        ] {
            assert_eq!(
                hash(&mut instance, &recorder, path, "", b"password"),
                expected.as_bytes(),
                "PBKDF2 must retain the ordinary parameter error"
            );
        }
    }

    fn keyed_hash_operations_compute_their_standard_values_and_disposal_prevents_reuse(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
        // Key "key", no data; the values are checked against the openssl command.
        for (path, expected) in [
            ("/hmac-clone", "5D5D139563C95B5967B9BD9A8C9B233A9DEDB45072794CD232DC1B74832607D0"),
            ("/hmac-current", "5D5D139563C95B5967B9BD9A8C9B233A9DEDB45072794CD232DC1B74832607D0"),
            ("/hmac-sha1", "F42BB0EEB018EBBD4597AE7213711EC60760843F"),
            ("/pbkdf-sha1", "4DB40A685139602EA72A3A5F490F3407C0DBAC8FBFE71A488F1933F20F368E42"),
        ] {
            assert_eq!(
                String::from_utf8_lossy(&hash(&mut instance, &recorder, path, "6b6579", b"")),
                expected,
                "a keyed hash operation must compute its standard value ({path})"
            );
        }
        assert_eq!(
            hash(&mut instance, &recorder, "/hmac-disposed", "6b6579", b""),
            b"ObjectDisposedException",
            "disposed keyed state must refuse further use"
        );
    }

    fn live_managed_hmac_state_survives_capture_and_restore(runtime: GuestRuntime) {
        let fixture = hashing(runtime);
        let (mut original, recorder) = fixture.activate();
        assert!(
            hash(&mut original, &recorder, "/hmac-start", "6b6579", b"a").is_empty(),
            "starting a digest must leave the keyed state unfinished"
        );
        let (mut restored, recorder) = fixture.restore_from(original, 0xAB);
        assert_eq!(
            hash(&mut restored, &recorder, "/hmac-finish", "", b"bc"),
            b"9C196E32DC0175F86F4B1CB89289D6619DE6BEE699E4C378E68309ED97A1A6AB",
            "restored incremental HMAC must retain the key and pre-capture prefix"
        );
    }

    fn ordinary_sha256_preserves_argument_disposal_and_algorithm_guards(runtime: GuestRuntime) {
        let (mut instance, recorder) = hashing(runtime).activate();
        // SHA-384 and incremental SHA-256 of nothing.
        let sha384_empty = "38B060A751AC96384CD9327EB1B1E36A21FDB71114BE07434C0CC7BF63F6E1DA274EDEBFE76F65FBD51AD2F14898B95B";
        let sha256_empty = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        for (path, expected) in [
            ("/sha-instance-null", "ArgumentNullException:buffer"),
            ("/sha-instance-offset", "ArgumentOutOfRangeException:offset"),
            ("/sha-instance-disposed", "ObjectDisposedException"),
            ("/sha-unsupported", sha384_empty),
            ("/sha-clone", sha256_empty),
            ("/sha-current", sha256_empty),
        ] {
            assert_eq!(
                String::from_utf8_lossy(&hash(&mut instance, &recorder, path, "", b"abc")),
                expected,
                "ordinary hash instances must preserve argument and lifetime guards ({path})"
            );
        }
    }

    fn unfinished_ordinary_sha256_state_survives_capture_and_restore(runtime: GuestRuntime) {
        let fixture = hashing(runtime);
        let (mut original, recorder) = fixture.activate();
        assert!(
            hash(&mut original, &recorder, "/sha-start", "", b"a").is_empty(),
            "starting SHA256 must leave the digest unfinished"
        );
        let (mut restored, recorder) = fixture.restore_from(original, 0xAB);
        assert_eq!(
            hash(&mut restored, &recorder, "/sha-finish", "", b"bc"),
            b"BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "restored SHA256 must retain the unfinished pre-capture prefix"
        );
    }
}
