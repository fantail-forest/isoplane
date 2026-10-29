//! The shipped root bundle, as X.509: every entry decodes and is a
//! self-signed root, and there are enough of them to be a public store.

use isoplane_test_support::{repository_root, run_successfully};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;

const SHIPPED_PEM: &[u8] = include_bytes!("../../../trust/ca-bundle.pem");

/// A public root store has held well over a hundred roots for years; a
/// bundle below this was truncated or mis-parsed, not curated.
const ROOT_FLOOR: usize = 100;

#[test]
fn the_shipped_bundle_is_a_store_of_self_signed_roots() {
    let mut roots = rustls::RootCertStore::empty();
    for (index, certificate) in CertificateDer::pem_slice_iter(SHIPPED_PEM).enumerate() {
        let certificate = certificate.expect("every PEM block in the shipped bundle decodes");
        roots
            .add(certificate)
            .unwrap_or_else(|error| panic!("certificate {index} is not an X.509 root: {error}"));
    }
    assert!(
        roots.len() >= ROOT_FLOOR,
        "a public root store holds at least {ROOT_FLOOR} roots; this bundle holds {}",
        roots.len()
    );
    let output =
        review_bundle(&repository_root(env!("CARGO_MANIFEST_DIR")).join("trust/ca-bundle.pem"));
    assert_eq!(
        output.matches("  self-signed  yes\n").count(),
        roots.len(),
        "OpenSSL must verify every shipped root's self-signature: {output}"
    );
    assert!(
        output.contains(&format!("{} added, 0 removed", roots.len())),
        "the review must parse every shipped certificate: {output}"
    );
}

#[test]
fn the_review_verifies_the_signature_of_a_self_issued_certificate() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trust");
    for (fixture, expected) in [
        ("self-signed-root.pem", "yes"),
        ("same-name-wrong-key.pem", "no"),
    ] {
        let output = review_bundle(&fixtures.join(fixture));
        assert!(
            output.contains(&format!("  self-signed  {expected}\n")),
            "{fixture} should be self-signed={expected}: {output}"
        );
        assert!(output.contains("1 added, 0 removed"), "{output}");
    }
}

fn review_bundle(bundle: &std::path::Path) -> String {
    let empty = tempfile::NamedTempFile::new().expect("create empty previous bundle");
    let script = repository_root(env!("CARGO_MANIFEST_DIR")).join("trust/review-bundle.sh");
    let output = run_successfully(
        std::process::Command::new("bash")
            .arg(script)
            .arg(empty.path())
            .arg(bundle),
        "review the bundle with bash and OpenSSL installed",
    );
    String::from_utf8(output.stdout).expect("review output is UTF-8")
}
