//! Cryptography through the pack's native OpenSSL backend (runtime patch
//! 009): the standard vectors hold, asymmetric primitives sign, verify and
//! agree, certificates parse and chain to a custom root, and randomness is
//! the host's entropy, fresh in every activation.

#[macro_use]
mod common;
#[path = "common/crypto_vectors.rs"]
mod crypto_vectors;

use common::{guest, reactor_without_egress, GuestRuntime};
use crypto_vectors::{
    AES_GCM_CIPHERTEXT, AES_GCM_TAG, HKDF_RFC5869_1, HMAC_JEFE, LEAF_SERIAL,
    LEAF_SIGNATURE_OF_WISP, LEAF_THUMBPRINT, PBKDF2_ONE, SHA256_ABC,
};

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    answer_at(runtime, owner, path, 1).await
}

/// The guest's wall clock is the dispatch's; a chain is verified at it.
async fn answer_at(runtime: GuestRuntime, owner: u64, path: &str, now_ms: u64) -> String {
    let reactor = reactor_without_egress(owner);
    reactor.context.begin_slice(now_ms, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

/// A wall-clock instant inside the fixed certificates' validity.
const WITHIN_VALIDITY_MS: u64 = 1_789_100_000_000;

in_both_runtimes! {
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

in_both_runtimes! {
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

in_both_runtimes! {
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

in_both_runtimes! {
    #[tokio::test]
    async fn aes_cbc_round_trips_a_generated_key(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 56, "/crypto/aes-cbc").await,
            "wisp acceptance plaintext True",
            "AES-256-CBC with a generated key and IV must round-trip and pad to whole blocks"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn rsa_signs_and_verifies_with_pkcs1_and_pss(runtime: GuestRuntime) {
        let answer = answer(runtime, 57, "/crypto/rsa-verify").await;
        assert_eq!(
            answer,
            format!("{LEAF_SIGNATURE_OF_WISP} True True False"),
            "the PKCS#1 signature must be the one OpenSSL computed over the same key, PSS must verify, and a tampered message must not"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn rsa_oaep_wraps_and_unwraps_a_secret(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 58, "/crypto/rsa-oaep").await,
            "a secret for the host 256",
            "OAEP-SHA256 over the 2048-bit leaf key must round-trip in a 256-byte ciphertext"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn rsa_generates_a_key_pair_that_signs(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 59, "/crypto/rsa-generate").await,
            "2048 True",
            "RSA key generation must yield a 2048-bit key whose signature verifies"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn ecdsa_signs_and_verifies_on_p256(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 60, "/crypto/ecdsa").await,
            "256 True False False",
            "a P-256 signature must verify with the public key, and neither a tampered message nor a foreign key may verify it"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn ecdh_derives_the_same_secret_on_both_sides(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 61, "/crypto/ecdh").await,
            "True 32",
            "ECDH on P-256 must agree on a 32-byte secret"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_certificate_parses_and_its_key_verifies_a_signature(runtime: GuestRuntime) {
        let answer = answer(runtime, 62, &format!("/crypto/x509-parse/{LEAF_SIGNATURE_OF_WISP}")).await;
        assert_eq!(
            answer,
            format!("CN=leaf.acceptance.test|{LEAF_THUMBPRINT}|{LEAF_SERIAL}|sha256RSA|True"),
            "the leaf must parse with its subject, thumbprint, serial and signature algorithm, and its key must verify the fixed signature"
        );
    }
}

in_both_runtimes! {
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

in_both_runtimes! {
    #[tokio::test]
    async fn a_certificate_signature_verifies_against_its_issuer(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 64, "/crypto/x509-sign-verify").await,
            "True",
            "the leaf's TBS certificate must verify under the root's public key"
        );
    }
}

in_both_runtimes! {
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
