//! The shipped root bundle, as X.509: every entry decodes and is a
//! self-signed root, and there are enough of them to be a public store.

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;

const SHIPPED_PEM: &[u8] = include_bytes!("../../../trust/ca-bundle.pem");

/// A public root store has held well over a hundred roots for years; a
/// bundle below this was truncated or mis-parsed, not curated.
const ROOT_FLOOR: usize = 100;

/// One DER TLV: tag, contents, the bytes after it.
fn tlv(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = bytes.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first & 0x80 == 0 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        let (digits, rest) = rest.split_at_checked(count)?;
        let length = digits.iter().try_fold(0usize, |acc, &d| {
            acc.checked_mul(256)?.checked_add(usize::from(d))
        })?;
        (length, rest)
    };
    let (contents, rest) = rest.split_at_checked(length)?;
    Some((tag, contents, rest))
}

/// The issuer and subject Names of a certificate, as raw DER.
fn issuer_and_subject(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let (0x30, certificate, _) = tlv(der)? else {
        return None;
    };
    let (0x30, mut tbs, _) = tlv(certificate)? else {
        return None;
    };
    let (tag, _, rest) = tlv(tbs)?;
    if tag == 0xa0 {
        tbs = rest;
    }
    let (0x02, _, rest) = tlv(tbs)? else {
        return None;
    };
    let (0x30, _, rest) = tlv(rest)? else {
        return None;
    };
    let issuer_start = rest;
    let (0x30, _, rest) = tlv(rest)? else {
        return None;
    };
    let issuer = &issuer_start[..issuer_start.len() - rest.len()];
    let (0x30, _, rest) = tlv(rest)? else {
        return None;
    };
    let subject_start = rest;
    let (0x30, _, rest) = tlv(rest)? else {
        return None;
    };
    let subject = &subject_start[..subject_start.len() - rest.len()];
    Some((issuer, subject))
}

#[test]
fn the_shipped_bundle_is_a_store_of_self_signed_roots() {
    let certificates: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(SHIPPED_PEM)
        .collect::<Result<_, _>>()
        .expect("every PEM block in the shipped bundle decodes");
    assert!(
        certificates.len() >= ROOT_FLOOR,
        "a public root store holds at least {ROOT_FLOOR} roots; this bundle holds {}",
        certificates.len()
    );
    for (index, certificate) in certificates.iter().enumerate() {
        let (issuer, subject) = issuer_and_subject(certificate)
            .unwrap_or_else(|| panic!("certificate {index} is not a DER X.509 certificate"));
        assert_eq!(
            issuer, subject,
            "certificate {index} is not self-signed: a root store carries roots only"
        );
    }
}

#[test]
fn the_der_walker_rejects_what_is_not_a_certificate() {
    assert_eq!(
        issuer_and_subject(b"not der"),
        None,
        "arbitrary bytes are not a certificate"
    );
    assert_eq!(
        issuer_and_subject(&[0x30, 0x02, 0x02, 0x00]),
        None,
        "a sequence holding an integer is not a certificate"
    );
}
