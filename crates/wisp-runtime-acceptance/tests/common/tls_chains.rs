//! The certificate chain a test TLS peer presents, from the fixed material.

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::tls_material::{
    CHAINED_CERTIFICATE, CHAINED_KEY, INTERMEDIATE_CERTIFICATE, LEAF_CERTIFICATE, LEAF_KEY,
    REMOTE_CERTIFICATE, REMOTE_KEY, ROOT_CERTIFICATE, STRANGER_CERTIFICATE, STRANGER_KEY,
};

/// The certificate a TLS peer presents.
#[derive(Clone, Copy)]
pub enum Certificate {
    /// The leaf for `leaf.acceptance.test`, presented with the root.
    Leaf,
    /// The leaf for `chained.acceptance.test`, presented with the
    /// intermediate that issued it; only the root is trusted.
    Chained,
    /// The leaf for `remote.acceptance.test`, whose issuer, CRL and OCSP
    /// responder are at URLs nothing answers, presented alone: the chain
    /// completes only through an AIA fetch.
    RemoteAlone,
    /// The same leaf presented with its intermediate: the chain is
    /// complete, revocation information is still only at the URLs.
    RemoteChained,
    /// A self-signed certificate nothing trusts.
    Stranger,
}

impl Certificate {
    /// The chain the peer presents, leaf first, and the leaf's private key.
    pub fn presented(self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let (chain, key) = match self {
            Certificate::Leaf => (format!("{LEAF_CERTIFICATE}\n{ROOT_CERTIFICATE}"), LEAF_KEY),
            Certificate::Chained => (
                format!("{CHAINED_CERTIFICATE}\n{INTERMEDIATE_CERTIFICATE}"),
                CHAINED_KEY,
            ),
            Certificate::RemoteAlone => (REMOTE_CERTIFICATE.to_owned(), REMOTE_KEY),
            Certificate::RemoteChained => (
                format!("{REMOTE_CERTIFICATE}\n{INTERMEDIATE_CERTIFICATE}"),
                REMOTE_KEY,
            ),
            Certificate::Stranger => (STRANGER_CERTIFICATE.to_owned(), STRANGER_KEY),
        };
        parse_chain(&chain, key)
    }
}

/// A PEM chain, leaf first, and its leaf's PEM key, as the peer's material.
pub fn parse_chain(
    chain: &str,
    key: &str,
) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let certificates = CertificateDer::pem_slice_iter(chain.as_bytes())
        .collect::<Result<_, _>>()
        .expect("the test material's certificates parse");
    let key =
        PrivateKeyDer::from_pem_slice(key.as_bytes()).expect("the test material's key parses");
    (certificates, key)
}
