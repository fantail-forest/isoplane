//! Cryptography through the pack's native OpenSSL backend (runtime patch
//! 009): the standard vectors hold, asymmetric primitives sign, verify and
//! agree, certificates parse and chain to a custom root, and randomness is
//! the host's entropy, fresh in every activation. The negatives: tampered
//! and malformed input is refused the way upstream .NET refuses it, key
//! material round-trips every format, and a disposed key is unusable.

#[macro_use]
mod common;
#[path = "common/crypto_vectors.rs"]
mod crypto_vectors;

use common::{guest, GuestRuntime};
use crypto_vectors::{
    AES_CCM_RFC3610_1_CIPHERTEXT, AES_CCM_RFC3610_1_TAG, AES_GCM_CIPHERTEXT, AES_GCM_TAG,
    CHACHA20_POLY1305_RFC8439_CIPHERTEXT, CHACHA20_POLY1305_RFC8439_TAG, HKDF_RFC5869_1, HMAC_JEFE,
    LEAF_SERIAL, LEAF_SIGNATURE_OF_ISOPLANE, LEAF_THUMBPRINT, PBKDF2_ONE, SHA256_ABC,
};

in_mono_runtime! {
    #[tokio::test]
    async fn encrypted_pkcs8_round_trips_and_refuses_the_wrong_password(runtime: GuestRuntime) {
        let text = answer(runtime, 60, "/crypto/encrypted-pkcs8").await;
        assert_eq!(
            text, "der=ok pem=ok wrong-password=CryptographicException",
            "an RSA key exported under a password (DER and PEM) imports back and verifies, and the wrong password is refused: {text}"
        );
    }

    #[tokio::test]
    async fn md5_matches_its_vector(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 61, "/crypto/md5").await,
            "900150983cd24fb0d6963f7d28e17f72",
            "MD5 of `abc` is RFC 1321's value"
        );
    }
}

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    answer_at(runtime, owner, path, 1).await
}

/// The guest's wall clock is the dispatch's; a chain is verified at it.
async fn answer_at(runtime: GuestRuntime, owner: u64, path: &str, now_ms: u64) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(owner));
    reactor.context.begin_slice(now_ms, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

/// A wall-clock instant inside the fixed certificates' validity.
const WITHIN_VALIDITY_MS: u64 = 1_789_100_000_000;

in_mono_runtime! {
    /// The AEAD ciphers exist only with a native backend, so this tells a
    /// build without one apart explicitly.
    #[tokio::test]
    async fn the_algorithms_are_backed_by_openssl(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 50, "/crypto/backend").await,
            "True True True",
            "AES-GCM, ChaCha20-Poly1305 and AES-CCM must be supported by the OpenSSL backend"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn digests_macs_and_key_derivation_match_their_standard_vectors(runtime: GuestRuntime) {
        for (index, (path, expected, what)) in [
            ("/crypto/sha256/abc", SHA256_ABC, "SHA-256"),
            ("/crypto/hmac", HMAC_JEFE, "HMAC-SHA256 (RFC 4231 case 2)"),
            (
                "/crypto/hkdf",
                HKDF_RFC5869_1,
                "HKDF-SHA256 (RFC 5869 case 1)",
            ),
            ("/crypto/pbkdf2", PBKDF2_ONE, "PBKDF2-HMAC-SHA256"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                answer(runtime, 51 + index as u64, path).await,
                expected,
                "{what} must match its published vector"
            );
        }
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_gcm_matches_the_nist_vector_and_round_trips(runtime: GuestRuntime) {
        let answer = answer(runtime, 55, "/crypto/aes-gcm").await;
        assert_eq!(
            answer,
            format!("{AES_GCM_CIPHERTEXT} {AES_GCM_TAG} {}", "00".repeat(16)),
            "AES-128-GCM must produce the NIST ciphertext and tag and decrypt them back"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_cbc_round_trips_a_generated_key(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 56, "/crypto/aes-cbc").await,
            "isoplane acceptance plaintext True",
            "AES-256-CBC with a generated key and IV must round-trip and pad to whole blocks"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn rsa_signs_and_verifies_with_pkcs1_and_pss(runtime: GuestRuntime) {
        let answer = answer(runtime, 57, "/crypto/rsa-verify").await;
        assert_eq!(
            answer,
            format!("{LEAF_SIGNATURE_OF_ISOPLANE} True True False"),
            "the PKCS#1 signature must be the one OpenSSL computed over the same key, PSS must verify, and a tampered message must not"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn rsa_oaep_wraps_and_unwraps_a_secret(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 58, "/crypto/rsa-oaep").await,
            "a secret for the host 256",
            "OAEP-SHA256 over the 2048-bit leaf key must round-trip in a 256-byte ciphertext"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn rsa_generates_a_key_pair_that_signs(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 59, "/crypto/rsa-generate").await,
            "2048 True",
            "RSA key generation must yield a 2048-bit key whose signature verifies"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn ecdsa_signs_and_verifies_on_p256(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 60, "/crypto/ecdsa").await,
            "256 True False False",
            "a P-256 signature must verify with the public key, and neither a tampered message nor a foreign key may verify it"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn ecdh_derives_the_same_secret_on_both_sides(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 61, "/crypto/ecdh").await,
            "True 32",
            "ECDH on P-256 must agree on a 32-byte secret"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_certificate_parses_and_its_key_verifies_a_signature(runtime: GuestRuntime) {
        let answer = answer(runtime, 62, &format!("/crypto/x509-parse/{LEAF_SIGNATURE_OF_ISOPLANE}")).await;
        assert_eq!(
            answer,
            format!("CN=leaf.acceptance.test|{LEAF_THUMBPRINT}|{LEAF_SERIAL}|sha256RSA|True"),
            "the leaf must parse with its subject, thumbprint, serial and signature algorithm, and its key must verify the fixed signature"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_certificate_chains_to_its_custom_root_and_not_to_a_stranger(runtime: GuestRuntime) {
        let answer = answer_at(runtime, 63, "/crypto/x509-chain", WITHIN_VALIDITY_MS).await;
        let mut parts = answer.split(' ');
        let (trusted, stranger) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        assert_eq!(
            trusted, "True:2:ok",
            "the leaf must chain to the root in the custom trust store: {answer}"
        );
        assert!(
            stranger.starts_with("False:"),
            "the leaf must not chain to an unrelated root: {answer}"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_certificate_signature_verifies_against_its_issuer(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 64, "/crypto/x509-sign-verify").await,
            "True",
            "the leaf's TBS certificate must verify under the root's public key"
        );
    }
}

in_mono_runtime! {
    /// Two activations of one template draw different random bytes: the
    /// generator draws from the host, never from a warm heap the clone copied.
    #[tokio::test]
    async fn randomness_is_fresh_in_every_activation(runtime: GuestRuntime) {
        let first = answer(runtime, 65, "/crypto/random").await;
        let second = answer(runtime, 66, "/crypto/random").await;
        let bytes = |answer: &str| answer.split(' ').next().unwrap_or("").to_string();
        assert_eq!(
            bytes(&first).len(),
            64,
            "32 random bytes must be answered: {first}"
        );
        assert_ne!(
            bytes(&first),
            bytes(&second),
            "two activations must not draw the same bytes"
        );
        assert_ne!(
            bytes(&first),
            "00".repeat(32),
            "the bytes must not be empty entropy"
        );
        for answer in [&first, &second] {
            let bounded: u32 = answer
                .split(' ')
                .nth(1)
                .and_then(|value| value.parse().ok())
                .expect("a bounded random integer");
            assert!(
                (1000..2000).contains(&bounded),
                "GetInt32 must respect its range: {answer}"
            );
        }
    }
}

/// The failure every AEAD reports for a nonce or tag one byte outside its
/// range and for an output buffer one byte shorter than its input.
const AEAD_LENGTH_FAILURES: &str = "nonce-short=ArgumentException nonce-long=ArgumentException tag-short=ArgumentException tag-long=ArgumentException output-short=ArgumentException";

/// Altering the tag, the ciphertext or the associated data of `algorithm`
/// fails its decryption as a tag mismatch, with the plaintext buffer cleared.
async fn assert_rejects_every_tampered_input(runtime: GuestRuntime, owner: u64, algorithm: &str) {
    for (index, input) in ["tag", "ciphertext", "associated-data"]
        .into_iter()
        .enumerate()
    {
        let answer = answer(
            runtime,
            owner + index as u64,
            &format!("/crypto/aead/{algorithm}/tampered-{input}"),
        )
        .await;
        assert_eq!(
            answer, "AuthenticationTagMismatchException True",
            "{algorithm} must fail a decryption whose {input} was altered as a tag mismatch and clear the plaintext buffer"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_gcm_rejects_every_tampered_input_and_clears_the_plaintext(runtime: GuestRuntime) {
        assert_rejects_every_tampered_input(runtime, 67, "aes-gcm").await;
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_gcm_refuses_invalid_key_nonce_tag_and_buffer_lengths(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 70, "/crypto/aead/aes-gcm/invalid-lengths").await,
            format!("key=CryptographicException {AEAD_LENGTH_FAILURES}"),
            "AES-GCM must refuse a 15-byte key, a nonce other than 12 bytes, a tag outside 12..16 bytes and a short output buffer"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_ccm_matches_the_rfc_3610_vector_and_round_trips(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 71, "/crypto/aead/aes-ccm/known-answer").await,
            format!("{AES_CCM_RFC3610_1_CIPHERTEXT} {AES_CCM_RFC3610_1_TAG} True"),
            "AES-128-CCM must produce the RFC 3610 ciphertext and tag and decrypt them back"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_ccm_rejects_every_tampered_input_and_clears_the_plaintext(runtime: GuestRuntime) {
        assert_rejects_every_tampered_input(runtime, 72, "aes-ccm").await;
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_ccm_refuses_invalid_key_nonce_tag_and_buffer_lengths(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 75, "/crypto/aead/aes-ccm/invalid-lengths").await,
            format!("key=CryptographicException {AEAD_LENGTH_FAILURES} tag-odd=ArgumentException"),
            "AES-CCM must refuse a 15-byte key, a nonce outside 7..13 bytes, a tag outside 4..16 bytes or of odd length, and a short output buffer"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn chacha20_poly1305_matches_the_rfc_8439_vector_and_round_trips(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 76, "/crypto/aead/chacha20-poly1305/known-answer").await,
            format!("{CHACHA20_POLY1305_RFC8439_CIPHERTEXT} {CHACHA20_POLY1305_RFC8439_TAG} True"),
            "ChaCha20-Poly1305 must produce the RFC 8439 ciphertext and tag and decrypt them back"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn chacha20_poly1305_rejects_every_tampered_input_and_clears_the_plaintext(runtime: GuestRuntime) {
        assert_rejects_every_tampered_input(runtime, 77, "chacha20-poly1305").await;
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn chacha20_poly1305_refuses_invalid_key_nonce_tag_and_buffer_lengths(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 80, "/crypto/aead/chacha20-poly1305/invalid-lengths").await,
            format!("key=CryptographicException {AEAD_LENGTH_FAILURES}"),
            "ChaCha20-Poly1305 must refuse a 31-byte key, a nonce other than 12 bytes, a tag other than 16 bytes and a short output buffer"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_cbc_rejects_invalid_padding_and_a_partial_block(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 81, "/crypto/aes-cbc-invalid-padding").await,
            "tampered=CryptographicException partial-block=CryptographicException intact=True",
            "AES-CBC must refuse a ciphertext whose PKCS#7 padding no longer decodes and one that is not whole blocks, and still decrypt the untouched one"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn aes_cbc_refuses_invalid_key_and_iv_lengths(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 82, "/crypto/aes-cbc-invalid-lengths").await,
            "key=CryptographicException key-size=CryptographicException iv=CryptographicException cbc-iv=ArgumentException",
            "AES must refuse a 15-byte key, a 100-bit key size, a 15-byte IV property and a 15-byte IV argument"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn rsa_oaep_rejects_a_tampered_ciphertext_and_an_oversized_plaintext(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 83, "/crypto/rsa-tampered-ciphertext").await,
            "tampered=CryptographicException truncated=CryptographicException too-large=CryptographicException",
            "RSA-OAEP must refuse an altered ciphertext, a truncated one and a plaintext the modulus cannot hold"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn ecdsa_verifies_false_on_a_malformed_signature(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 84, "/crypto/ecdsa-malformed-signatures").await,
            "empty=False truncated=False altered=False intact=True",
            "an empty, truncated or altered P-256 signature must verify false without throwing, and the intact one true"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn rsa_keys_round_trip_through_every_import_and_export_format(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 85, "/crypto/rsa-import-export").await,
            "pkcs8=True/True/True pkcs1=True/True/True spki=True/CryptographicException rsa-public=True/CryptographicException pkcs8-pem=True/True/True pkcs1-pem=True/True/True spki-pem=True/CryptographicException rsa-public-pem=True/CryptographicException parameters=True/True/True",
            "every private RSA format must import a key that verifies, signs and carries the modulus, and every public one a key that verifies but cannot export a private key"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn ecdsa_keys_round_trip_through_every_import_and_export_format(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 87, "/crypto/ecdsa-import-export").await,
            "pkcs8=True/True/True ec-private=True/True/True spki=True/CryptographicException pkcs8-pem=True/True/True ec-private-pem=True/True/True spki-pem=True/CryptographicException parameters=True/True/True",
            "every private ECDSA format must import a key that verifies, signs and carries the public point, and every public one a key that verifies but cannot export a private key"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn ecdh_keys_round_trip_through_every_import_and_export_format(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 88, "/crypto/ecdh-import-export").await,
            "pkcs8=True/True ec-private=True/True spki=True/CryptographicException public-key-spki=True/CryptographicException pkcs8-pem=True/True spki-pem=True/CryptographicException parameters=True/True",
            "every private ECDH format must import a key that agrees from both sides, and every public one a key the peer agrees with but that cannot export a private key"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn malformed_key_material_is_refused_on_import(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 89, "/crypto/malformed-key-imports").await,
            "garbage-pkcs8=CryptographicException garbage-spki=CryptographicException truncated-pkcs8=CryptographicException spki-as-pkcs8=CryptographicException ec-key-as-rsa=CryptographicException rsa-key-as-ec=CryptographicException rsa-pem-as-ec=CryptographicException not-pem=ArgumentException certificate-pem=ArgumentException",
            "bytes that are not a key, a truncated key, a key of the other algorithm and a public key in a private slot must fail as cryptographic errors, and text that is not a key PEM as an argument error"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_disposed_key_refuses_every_operation(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 90, "/crypto/disposed-keys").await,
            "rsa-pkcs8=ObjectDisposedException rsa-spki=ObjectDisposedException rsa-pem=ObjectDisposedException rsa-sign=ObjectDisposedException ecdsa-pkcs8=ObjectDisposedException ecdsa-spki=ObjectDisposedException ecdsa-sign=ObjectDisposedException ecdh-pkcs8=ObjectDisposedException ecdh-derive=ObjectDisposedException",
            "a disposed RSA, ECDSA or ECDH key must refuse to export, sign or agree with ObjectDisposedException, and disposing twice must not throw"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn disposing_a_key_leaves_its_copies_usable(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 91, "/crypto/disposal-is-per-instance").await,
            "copy=True exported=True certificate=True",
            "disposing an RSA key must leave a key built from its parameters, its exported bytes and a fresh key from its certificate usable"
        );
    }
}
