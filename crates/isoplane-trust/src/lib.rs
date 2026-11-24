#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where a guest finds the bundle.
pub const GUEST_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";

const SHIPPED_PEM: &[u8] = include_bytes!("../../../trust/ca-bundle.pem");
const SHIPPED_MANIFEST: &str = include_str!("../../../trust/ca-bundle.manifest.json");
const SHIPPED_MANIFEST_NAME: &str = "trust/ca-bundle.manifest.json";

/// The earliest time a bundle without a manifest reports: 2000-01-01T00:00:00Z.
pub const MODIFIED_EPOCH_UNIX_SECS: u64 = 946_684_800;
/// The length of the range those times fall in: fifty years of 365 days.
pub const MODIFIED_SPAN_SECS: u64 = 50 * 365 * 86_400;

/// The `[trust]` section of `isoplaned.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// A PEM bundle served instead of the one shipped with the build.
    /// `sha256` is required with it. A manifest written by
    /// `trust/ingest-bundle.sh` beside it, named `<stem>.manifest.json`,
    /// supplies the time guests see on the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
    /// Hex SHA-256 the operator bundle must hash to. Ignored for the
    /// shipped bundle, whose manifest carries its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// The fields the node reads from a manifest `trust/ingest-bundle.sh`
/// writes. The provenance fields the script also records are not read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
pub struct Manifest {
    /// SHA-256 of the bundle as fetched.
    pub bundle_sha256: String,
    pub certificate_count: u32,
    /// The header's Mozilla timestamp, verbatim.
    pub mozilla_as_of: String,
}

/// Which bundle a node serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustSource {
    Shipped,
    Operator(PathBuf),
    /// A developer machine's own roots, followed by the shipped ones.
    LocalDevelopment,
}

impl std::fmt::Display for TrustSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustSource::Shipped => write!(f, "shipped with this build (trust/ca-bundle.pem)"),
            TrustSource::Operator(path) => write!(f, "`{}` (trust.bundle)", path.display()),
            TrustSource::LocalDevelopment => {
                write!(f, "this machine's trusted roots and the shipped bundle")
            }
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error(
        "trust.bundle = `{path}` is set without trust.sha256: an operator bundle is served \
         only with the hash it must match"
    )]
    Sha256Missing { path: PathBuf },
    #[error(
        "trust.sha256 is set without trust.bundle: the shipped bundle is checked against its \
         own manifest, so there is nothing for this hash to check"
    )]
    BundleMissing,
    #[error("trust.sha256 `{value}` is not 64 hexadecimal characters")]
    Sha256Malformed { value: String },
    #[error("trust bundle `{path}` cannot be read: {error}")]
    Unreadable { path: PathBuf, error: String },
    #[error(
        "trust bundle {bundle} hashes to sha256 {actual}, but {recorded_by} records {expected}: \
         refusing to serve bytes that are not the ones recorded"
    )]
    Sha256Mismatch {
        bundle: TrustSource,
        expected: String,
        actual: String,
        recorded_by: String,
    },
    #[error("trust manifest {path} is malformed: {error}")]
    ManifestMalformed { path: String, error: String },
    #[error(
        "trust manifest {path} records {manifest} certificates but the bundle holds {bundle}: \
         the manifest does not describe this bundle"
    )]
    CountMismatch {
        path: String,
        manifest: u32,
        bundle: usize,
    },
    #[error("trust bundle {bundle} contains an invalid certificate at entry {entry}: {error}")]
    InvalidCertificate {
        bundle: TrustSource,
        entry: usize,
        error: String,
    },
    #[error("trust bundle {bundle} contains no certificates")]
    NoCertificates { bundle: TrustSource },
    #[error(
        "trust manifest {path} has mozilla_as_of `{value}`, which is not a timestamp of the form \
         `Thu Aug 13 03:12:01 2026 GMT`"
    )]
    MozillaAsOfMalformed { path: String, value: String },
}

/// A verified bundle and what is known about it.
#[derive(Debug, Clone)]
pub struct TrustBundle {
    pem: Vec<u8>,
    sha256: [u8; 32],
    /// The manifest's `mozilla_as_of`.
    published_unix_secs: Option<u64>,
}

impl TrustBundle {
    /// Read, hash and check the bundle `cfg` names, or the shipped one.
    pub fn load(cfg: &TrustConfig) -> Result<Self, TrustError> {
        match (&cfg.bundle, &cfg.sha256) {
            (None, None) => Self::shipped(),
            (Some(path), Some(sha)) => Self::operator(path, sha),
            (Some(path), None) => Err(TrustError::Sha256Missing { path: path.clone() }),
            (None, Some(_)) => Err(TrustError::BundleMissing),
        }
    }

    /// The roots a local development session serves: the developer machine's
    /// own, as its tools exported them, followed by the shipped public roots.
    /// A Windows certificate store holds only the roots that machine has
    /// already needed, so a machine's roots alone can leave a common public
    /// authority untrusted. The guest's TLS decides what each root is good
    /// for; a machine root this host's own verifier would refuse is served.
    pub fn local_development(machine_pem: &[u8]) -> Result<Self, TrustError> {
        let source = TrustSource::LocalDevelopment;
        let mut pem = machine_pem.to_vec();
        if !pem.ends_with(b"\n") {
            pem.push(b'\n');
        }
        pem.extend_from_slice(SHIPPED_PEM);
        for (index, certificate) in CertificateDer::pem_slice_iter(&pem).enumerate() {
            certificate.map_err(|error| TrustError::InvalidCertificate {
                bundle: source.clone(),
                entry: index + 1,
                error: error.to_string(),
            })?;
        }
        Ok(TrustBundle {
            sha256: Sha256::digest(&pem).into(),
            pem,
            published_unix_secs: None,
        })
    }

    fn shipped() -> Result<Self, TrustError> {
        let manifest = parse_manifest(SHIPPED_MANIFEST_NAME, SHIPPED_MANIFEST)?;
        let expected = manifest.bundle_sha256.clone();
        Self::checked(
            SHIPPED_PEM.to_vec(),
            TrustSource::Shipped,
            &expected,
            SHIPPED_MANIFEST_NAME,
            Some((SHIPPED_MANIFEST_NAME.to_string(), manifest)),
        )
    }

    fn operator(path: &Path, sha: &str) -> Result<Self, TrustError> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TrustError::Sha256Malformed {
                value: sha.to_string(),
            });
        }
        let pem = std::fs::read(path).map_err(|e| TrustError::Unreadable {
            path: path.to_path_buf(),
            error: e.to_string(),
        })?;
        let manifest_path = path.with_extension("manifest.json");
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => Some((
                manifest_path.display().to_string(),
                parse_manifest(&manifest_path.display().to_string(), &text)?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(TrustError::Unreadable {
                    path: manifest_path,
                    error: e.to_string(),
                })
            }
        };
        Self::checked(
            pem,
            TrustSource::Operator(path.to_path_buf()),
            sha,
            "trust.sha256",
            manifest,
        )
    }

    fn checked(
        pem: Vec<u8>,
        source: TrustSource,
        expected_sha256: &str,
        recorded_by: &str,
        manifest: Option<(String, Manifest)>,
    ) -> Result<Self, TrustError> {
        let sha256: [u8; 32] = Sha256::digest(&pem).into();
        let actual = hex::encode(sha256);
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(TrustError::Sha256Mismatch {
                bundle: source,
                expected: expected_sha256.to_ascii_lowercase(),
                actual,
                recorded_by: recorded_by.to_string(),
            });
        }
        let certificate_count = validate_certificates(&pem, &source)?;
        if certificate_count == 0 {
            return Err(TrustError::NoCertificates { bundle: source });
        }
        let mut published_unix_secs = None;
        if let Some((path, manifest)) = manifest {
            if !manifest.bundle_sha256.eq_ignore_ascii_case(&actual) {
                return Err(TrustError::Sha256Mismatch {
                    bundle: source,
                    expected: manifest.bundle_sha256.to_ascii_lowercase(),
                    actual,
                    recorded_by: path,
                });
            }
            if manifest.certificate_count as usize != certificate_count {
                return Err(TrustError::CountMismatch {
                    path,
                    manifest: manifest.certificate_count,
                    bundle: certificate_count,
                });
            }
            published_unix_secs = Some(parse_header_time(&manifest.mozilla_as_of).ok_or(
                TrustError::MozillaAsOfMalformed {
                    path,
                    value: manifest.mozilla_as_of,
                },
            )?);
        }
        Ok(TrustBundle {
            pem,
            sha256,
            published_unix_secs,
        })
    }

    /// The bytes a guest reads.
    pub fn pem(&self) -> &[u8] {
        &self.pem
    }

    /// The modification time guests see on the file, in nanoseconds since
    /// the Unix epoch.
    ///
    /// A bundle with a manifest reports when its certificate data was taken
    /// from Mozilla, [`Manifest::mozilla_as_of`]. A bundle without one
    /// reports a value derived from its content, which is not a date. Either
    /// way the same bundle reports the same time on every node. A guest's
    /// runtime loads the roots again when the time changes, and two
    /// different bundles can report the same time.
    pub fn modified_ns(&self) -> u64 {
        match self.published_unix_secs {
            Some(secs) => secs * 1_000_000_000,
            None => content_modified_ns(&self.sha256),
        }
    }
}

/// The time a bundle with no publication date reports; see [`TrustBundle::modified_ns`].
fn content_modified_ns(sha256: &[u8; 32]) -> u64 {
    let mut head = [0u8; 8];
    head.copy_from_slice(&sha256[..8]);
    let offset_secs = u64::from_be_bytes(head) % MODIFIED_SPAN_SECS;
    (MODIFIED_EPOCH_UNIX_SECS + offset_secs) * 1_000_000_000
}

fn parse_manifest(path: &str, text: &str) -> Result<Manifest, TrustError> {
    serde_json::from_str(text).map_err(|e| TrustError::ManifestMalformed {
        path: path.to_string(),
        error: e.to_string(),
    })
}

fn validate_certificates(pem: &[u8], source: &TrustSource) -> Result<usize, TrustError> {
    let mut roots = rustls::RootCertStore::empty();
    for (index, certificate) in CertificateDer::pem_slice_iter(pem).enumerate() {
        let certificate = certificate.map_err(|error| TrustError::InvalidCertificate {
            bundle: source.clone(),
            entry: index + 1,
            error: error.to_string(),
        })?;
        roots
            .add(certificate)
            .map_err(|error| TrustError::InvalidCertificate {
                bundle: source.clone(),
                entry: index + 1,
                error: error.to_string(),
            })?;
    }
    Ok(roots.len())
}

/// The timestamp a bundle's header carries, `Thu Aug 13 03:12:01 2026 GMT`,
/// to seconds since the Unix epoch.
fn parse_header_time(text: &str) -> Option<u64> {
    let time = httpdate::parse_http_date(text.strip_suffix(" GMT")?).ok()?;
    Some(time.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate_count(bundle: &TrustBundle) -> usize {
        CertificateDer::pem_slice_iter(bundle.pem()).count()
    }

    fn shipped_manifest() -> Manifest {
        parse_manifest(SHIPPED_MANIFEST_NAME, SHIPPED_MANIFEST).expect("the shipped manifest")
    }

    fn operator_dir(pem: &[u8], manifest: Option<&Manifest>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("roots.pem");
        std::fs::write(&path, pem).expect("write the bundle");
        if let Some(manifest) = manifest {
            std::fs::write(
                dir.path().join("roots.manifest.json"),
                serde_json::to_string(manifest).expect("a manifest serialises"),
            )
            .expect("write the manifest");
        }
        (dir, path)
    }

    fn operator_config(path: &Path, sha256: &str) -> TrustConfig {
        TrustConfig {
            bundle: Some(path.to_path_buf()),
            sha256: Some(sha256.to_string()),
        }
    }

    #[test]
    fn a_local_development_bundle_serves_the_machine_roots_before_the_shipped_ones() {
        let shipped = TrustBundle::load(&TrustConfig::default()).expect("the shipped bundle loads");
        let first = SHIPPED_PEM
            .windows(b"-----END CERTIFICATE-----".len())
            .position(|window| window == b"-----END CERTIFICATE-----")
            .expect("the shipped bundle holds a certificate")
            + b"-----END CERTIFICATE-----".len();
        let machine = &SHIPPED_PEM[..first];
        let bundle = TrustBundle::local_development(machine).expect("a machine export loads");
        assert!(
            bundle.pem().starts_with(machine) && bundle.pem().ends_with(SHIPPED_PEM),
            "the machine's roots and every shipped root must both be served"
        );
        assert_eq!(
            certificate_count(&bundle),
            certificate_count(&shipped) + 1,
            "each served certificate is counted, including one both sources hold"
        );
        assert_ne!(
            bundle.modified_ns(),
            shipped.modified_ns(),
            "the local development bundle and the shipped one report different modification times"
        );
    }

    #[test]
    fn a_local_development_bundle_refuses_a_malformed_machine_export() {
        let malformed = b"-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----\n";
        assert!(
            matches!(
                TrustBundle::local_development(malformed),
                Err(TrustError::InvalidCertificate {
                    bundle: TrustSource::LocalDevelopment,
                    entry: 1,
                    ..
                })
            ),
            "a damaged export must be reported, not served"
        );
    }

    #[test]
    fn the_manifest_parser_reads_its_fields_from_what_the_ingest_script_writes() {
        let manifest = parse_manifest(
            "m",
            r#"{"generation": 3, "bundle_date": "2026-08-13",
                "source_url": "https://curl.se/ca/cacert-2026-08-13.pem",
                "bundle_sha256": "f66d", "certdata_sha256": "81b7",
                "mk_ca_bundle_version": "1.33", "certificate_count": 121,
                "mozilla_as_of": "Thu Aug 13 03:12:01 2026 GMT",
                "ingested_at": "2026-09-11T05:39:35Z"}"#,
        )
        .expect("a complete manifest parses");
        assert_eq!(
            manifest,
            Manifest {
                bundle_sha256: "f66d".into(),
                certificate_count: 121,
                mozilla_as_of: "Thu Aug 13 03:12:01 2026 GMT".into(),
            }
        );
    }

    #[test]
    fn an_operator_bundle_whose_hash_differs_is_refused() {
        let (_dir, path) = operator_dir(SHIPPED_PEM, None);
        let wrong = "0".repeat(64);
        let err = TrustBundle::load(&operator_config(&path, &wrong))
            .expect_err("a bundle that is not the one recorded must not be served");
        assert_eq!(
            err,
            TrustError::Sha256Mismatch {
                bundle: TrustSource::Operator(path.clone()),
                expected: wrong,
                actual: shipped_manifest().bundle_sha256,
                recorded_by: "trust.sha256".into(),
            },
            "the refusal names both hashes and where the expected one came from"
        );
    }

    #[test]
    fn an_operator_bundle_without_its_hash_is_refused() {
        let (_dir, path) = operator_dir(SHIPPED_PEM, None);
        let cfg = TrustConfig {
            bundle: Some(path.clone()),
            sha256: None,
        };
        assert_eq!(
            TrustBundle::load(&cfg).expect_err("no hash, no bundle"),
            TrustError::Sha256Missing { path },
            "an operator bundle is served only with the hash it must match"
        );
    }

    #[test]
    fn a_hash_without_a_bundle_is_refused() {
        let cfg = TrustConfig {
            bundle: None,
            sha256: Some("0".repeat(64)),
        };
        assert_eq!(
            TrustBundle::load(&cfg).expect_err("a hash with nothing to check"),
            TrustError::BundleMissing,
            "trust.sha256 applies only to trust.bundle"
        );
    }

    #[test]
    fn a_malformed_hash_is_refused_before_the_file_is_read() {
        let cfg = TrustConfig {
            bundle: Some(PathBuf::from("/nonexistent/roots.pem")),
            sha256: Some("abc".into()),
        };
        assert_eq!(
            TrustBundle::load(&cfg).expect_err("three hex digits are not a sha256"),
            TrustError::Sha256Malformed {
                value: "abc".into()
            },
            "the hash is checked for shape first"
        );
    }

    #[test]
    fn an_operator_bundle_with_the_right_hash_is_served_with_its_manifest() {
        let manifest = shipped_manifest();
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let bundle = TrustBundle::load(&operator_config(&path, &manifest.bundle_sha256))
            .expect("a matching operator bundle loads");
        assert_eq!(
            bundle.pem(),
            SHIPPED_PEM,
            "the operator's bytes are the ones served"
        );
    }

    #[test]
    fn a_sibling_manifest_written_for_other_bytes_is_refused() {
        let mut manifest = shipped_manifest();
        manifest.bundle_sha256 = "1".repeat(64);
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let real = shipped_manifest().bundle_sha256;
        let err = TrustBundle::load(&operator_config(&path, &real))
            .expect_err("a manifest that describes different bytes must not be trusted");
        assert!(
            matches!(&err, TrustError::Sha256Mismatch { recorded_by, .. } if recorded_by.ends_with("roots.manifest.json")),
            "the refusal names the manifest as the source of the expectation: {err}"
        );
    }

    #[test]
    fn a_manifest_whose_count_does_not_describe_the_bundle_is_refused() {
        let mut manifest = shipped_manifest();
        manifest.certificate_count += 1;
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let err = TrustBundle::load(&operator_config(&path, &manifest.bundle_sha256))
            .expect_err("a count that does not match is a manifest for another bundle");
        assert!(
            matches!(err, TrustError::CountMismatch { manifest: m, bundle: b, .. } if m == b as u32 + 1),
            "the refusal carries both counts: {err}"
        );
    }

    #[test]
    fn a_bundle_with_no_certificates_is_refused() {
        let pem = b"## a header and nothing else\n";
        let sha = hex::encode(Sha256::digest(pem));
        let (_dir, path) = operator_dir(pem, None);
        assert_eq!(
            TrustBundle::load(&operator_config(&path, &sha)).expect_err("an empty store"),
            TrustError::NoCertificates {
                bundle: TrustSource::Operator(path)
            },
            "an empty trust store would make every TLS connection fail; refuse it at startup"
        );
    }

    #[test]
    fn a_bundle_header_without_a_certificate_is_refused_even_with_the_right_hash() {
        let pem = b"-----BEGIN CERTIFICATE-----\n";
        let sha = hex::encode(Sha256::digest(pem));
        let (_dir, path) = operator_dir(pem, None);
        assert!(
            TrustBundle::load(&operator_config(&path, &sha)).is_err(),
            "integrity of a PEM header does not make it a usable trust store"
        );
    }

    #[test]
    fn a_malformed_der_certificate_is_refused_even_with_the_right_hash() {
        let pem = b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n";
        let sha = hex::encode(Sha256::digest(pem));
        let (_dir, path) = operator_dir(pem, None);
        assert!(
            TrustBundle::load(&operator_config(&path, &sha)).is_err(),
            "base64 bytes that are not a certificate must fail at startup"
        );
    }

    #[test]
    fn a_valid_bundle_followed_by_a_truncated_pem_entry_is_refused() {
        let mut pem = SHIPPED_PEM.to_vec();
        pem.extend_from_slice(b"\n-----BEGIN CERTIFICATE-----\n");
        let sha = hex::encode(Sha256::digest(&pem));
        let (_dir, path) = operator_dir(&pem, None);
        assert!(
            TrustBundle::load(&operator_config(&path, &sha)).is_err(),
            "a usable prefix must not conceal an incomplete certificate entry"
        );
    }

    #[test]
    fn a_valid_bundle_followed_by_a_malformed_certificate_is_refused() {
        let mut pem = SHIPPED_PEM.to_vec();
        pem.extend_from_slice(b"\n-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n");
        let sha = hex::encode(Sha256::digest(&pem));
        let (_dir, path) = operator_dir(&pem, None);
        assert!(
            TrustBundle::load(&operator_config(&path, &sha)).is_err(),
            "one usable root must not hide another malformed certificate"
        );
    }

    #[test]
    fn an_unreadable_bundle_names_its_path() {
        let cfg = TrustConfig {
            bundle: Some(PathBuf::from("/nonexistent/roots.pem")),
            sha256: Some("0".repeat(64)),
        };
        let err = TrustBundle::load(&cfg).expect_err("a missing file cannot be served");
        assert!(
            matches!(&err, TrustError::Unreadable { path, .. } if path == Path::new("/nonexistent/roots.pem")),
            "the refusal names the file: {err}"
        );
    }

    #[test]
    fn a_bundle_with_a_manifest_reports_the_second_its_data_was_taken() {
        let mut manifest = shipped_manifest();
        manifest.mozilla_as_of = "Thu Jan  2 03:04:05 2025 GMT".into();
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let bundle = TrustBundle::load(&operator_config(&path, &manifest.bundle_sha256))
            .expect("an operator bundle with a manifest loads");
        assert_eq!(bundle.modified_ns(), 1_735_787_045 * 1_000_000_000);
    }

    #[test]
    fn a_manifest_whose_mozilla_timestamp_cannot_be_read_is_refused() {
        let mut manifest = shipped_manifest();
        manifest.mozilla_as_of = "last Tuesday".into();
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        assert!(matches!(
            TrustBundle::load(&operator_config(&path, &manifest.bundle_sha256)),
            Err(TrustError::MozillaAsOfMalformed { value, .. }) if value == "last Tuesday"
        ));
    }

    #[test]
    fn a_bundle_without_a_manifest_reports_a_time_derived_from_its_content() {
        let a: [u8; 32] = Sha256::digest(b"bundle a").into();
        let b: [u8; 32] = Sha256::digest(b"bundle b").into();
        assert_ne!(
            content_modified_ns(&a),
            content_modified_ns(&b),
            "these two bundles report different times, so replacing one with the other is visible"
        );
        let secs = content_modified_ns(&a) / 1_000_000_000;
        assert!(
            (MODIFIED_EPOCH_UNIX_SECS..MODIFIED_EPOCH_UNIX_SECS + MODIFIED_SPAN_SECS)
                .contains(&secs),
            "the timestamp lies within fifty years of 2000-01-01"
        );
        assert_eq!(
            content_modified_ns(&a) % 1_000_000_000,
            0,
            "the derivation is in whole seconds"
        );
        let (_dir, path) = operator_dir(SHIPPED_PEM, None);
        let bundle = TrustBundle::load(&operator_config(&path, &shipped_manifest().bundle_sha256))
            .expect("an operator bundle without a manifest loads");
        assert_eq!(
            bundle.modified_ns(),
            content_modified_ns(&Sha256::digest(SHIPPED_PEM).into())
        );
    }
}
