//! The platform-managed public TLS root bundle: the bytes every guest reads
//! at [`GUEST_PATH`], checked against the manifest ingested with them, and
//! the facts the node reports about them.
//!
//! The bundle is embedded at build time from `trust/`, so rotating it is a
//! node release and a node cannot start without one. An operator may serve a
//! different bundle by path, but only together with the hash it must match.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where a guest finds the bundle.
pub const GUEST_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";

const SHIPPED_PEM: &[u8] = include_bytes!("../../../trust/ca-bundle.pem");
const SHIPPED_MANIFEST: &str = include_str!("../../../trust/ca-bundle.manifest.json");
const SHIPPED_MANIFEST_NAME: &str = "trust/ca-bundle.manifest.json";

/// The origin of the timestamp guests see on the bundle: 2000-01-01T00:00:00Z.
pub const MODIFIED_EPOCH_UNIX_SECS: u64 = 946_684_800;
/// The span the content-derived offset is folded into: fifty years of 365 days.
pub const MODIFIED_SPAN_SECS: u64 = 50 * 365 * 86_400;

const SECS_PER_DAY: u64 = 86_400;
const PEM_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";

fn default_warn_after_days() -> u32 {
    30
}

/// The `[trust]` section of `wispd.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// A PEM bundle served instead of the one shipped with the build.
    /// `sha256` is required with it. A manifest written by
    /// `trust/ingest-bundle.sh` beside it, named `<stem>.manifest.json`,
    /// supplies the bundle's date, generation and last-check age; without
    /// one the node cannot say how old the bundle is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<PathBuf>,
    /// Hex SHA-256 the operator bundle must hash to. Ignored for the
    /// shipped bundle, whose manifest carries its hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Days since the last successful upstream check after which startup
    /// warns. Serving continues either way.
    #[serde(default = "default_warn_after_days")]
    pub warn_after_days: u32,
}

impl Default for TrustConfig {
    fn default() -> Self {
        TrustConfig {
            bundle: None,
            sha256: None,
            warn_after_days: default_warn_after_days(),
        }
    }
}

/// What `trust/ingest-bundle.sh` records about a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Goes up by one on every change to the bundle bytes, rollbacks included.
    pub generation: u64,
    /// The date in curl's file name.
    pub bundle_date: String,
    pub source_url: String,
    /// SHA-256 of the bundle as fetched.
    pub bundle_sha256: String,
    /// The hash of Mozilla's `certdata.txt` named in the bundle header.
    pub certdata_sha256: String,
    pub mk_ca_bundle_version: String,
    pub certificate_count: u32,
    /// The header's Mozilla timestamp, verbatim.
    pub mozilla_as_of: String,
    /// The last successful upstream check, `YYYY-MM-DDTHH:MM:SSZ`.
    pub ingested_at: String,
}

/// Which bundle a node serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustSource {
    Shipped,
    Operator(PathBuf),
}

impl std::fmt::Display for TrustSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustSource::Shipped => write!(f, "shipped with this build (trust/ca-bundle.pem)"),
            TrustSource::Operator(path) => write!(f, "`{}` (trust.bundle)", path.display()),
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
    #[error("trust bundle {bundle} contains no certificates")]
    NoCertificates { bundle: TrustSource },
    #[error(
        "trust manifest {path} has ingested_at `{value}`, which is not a UTC timestamp of the \
         form YYYY-MM-DDTHH:MM:SSZ"
    )]
    IngestedAtMalformed { path: String, value: String },
}

/// The facts a node reports about the bundle it serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustFacts {
    /// `shipped`, or the operator's path.
    pub source: String,
    pub bundle_date: Option<String>,
    pub generation: Option<u64>,
    /// Full hex SHA-256 of the served bytes.
    pub sha256: String,
    pub certificate_count: usize,
    /// The last successful upstream check, when a manifest records one.
    pub ingested_at: Option<String>,
}

impl TrustFacts {
    /// Enough of the hash to tell two bundles apart in a log line.
    pub fn sha256_prefix(&self) -> &str {
        &self.sha256[..12]
    }
}

/// How long since the bundle was last checked against upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    Fresh {
        age_days: u64,
    },
    /// Past `warn_after_days`: a warning, never a refusal.
    Stale {
        age_days: u64,
        warn_after_days: u32,
        last_check: String,
        bundle_date: String,
    },
    /// No manifest records a check: an operator bundle without one.
    Unknown,
}

impl std::fmt::Display for Staleness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Staleness::Fresh { age_days } => {
                write!(f, "last upstream check {age_days} days ago")
            }
            Staleness::Stale {
                age_days,
                warn_after_days,
                last_check,
                bundle_date,
            } => write!(
                f,
                "the trust bundle's last successful upstream check was {last_check}, {age_days} \
                 days ago, past trust.warn_after_days = {warn_after_days}; the bundle itself is \
                 dated {bundle_date}, which is its publication date, not the check. Re-run \
                 trust/ingest-bundle.sh and release, or point trust.bundle at a newer bundle. \
                 Serving continues"
            ),
            Staleness::Unknown => write!(
                f,
                "no manifest records when this trust bundle was last checked against upstream, \
                 so its age cannot be judged; serving continues"
            ),
        }
    }
}

/// A verified bundle and what is known about it.
#[derive(Debug, Clone)]
pub struct TrustBundle {
    pem: Vec<u8>,
    sha256: [u8; 32],
    certificate_count: usize,
    source: TrustSource,
    manifest: Option<Manifest>,
    last_check_unix_secs: Option<u64>,
    warn_after_days: u32,
}

impl TrustBundle {
    /// Read, hash and check the bundle `cfg` names, or the shipped one.
    pub fn load(cfg: &TrustConfig) -> Result<Self, TrustError> {
        match (&cfg.bundle, &cfg.sha256) {
            (None, None) => Self::shipped(cfg.warn_after_days),
            (Some(path), Some(sha)) => Self::operator(path, sha, cfg.warn_after_days),
            (Some(path), None) => Err(TrustError::Sha256Missing { path: path.clone() }),
            (None, Some(_)) => Err(TrustError::BundleMissing),
        }
    }

    fn shipped(warn_after_days: u32) -> Result<Self, TrustError> {
        let manifest = parse_manifest(SHIPPED_MANIFEST_NAME, SHIPPED_MANIFEST)?;
        let expected = manifest.bundle_sha256.clone();
        Self::checked(
            SHIPPED_PEM.to_vec(),
            TrustSource::Shipped,
            &expected,
            SHIPPED_MANIFEST_NAME,
            Some((SHIPPED_MANIFEST_NAME.to_string(), manifest)),
            warn_after_days,
        )
    }

    fn operator(path: &Path, sha: &str, warn_after_days: u32) -> Result<Self, TrustError> {
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
            warn_after_days,
        )
    }

    fn checked(
        pem: Vec<u8>,
        source: TrustSource,
        expected_sha256: &str,
        recorded_by: &str,
        manifest: Option<(String, Manifest)>,
        warn_after_days: u32,
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
        let certificate_count = count_certificates(&pem);
        if certificate_count == 0 {
            return Err(TrustError::NoCertificates { bundle: source });
        }
        let mut last_check_unix_secs = None;
        let manifest = match manifest {
            Some((path, manifest)) => {
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
                last_check_unix_secs = Some(parse_utc(&manifest.ingested_at).ok_or_else(|| {
                    TrustError::IngestedAtMalformed {
                        path,
                        value: manifest.ingested_at.clone(),
                    }
                })?);
                Some(manifest)
            }
            None => None,
        };
        Ok(TrustBundle {
            pem,
            sha256,
            certificate_count,
            source,
            manifest,
            last_check_unix_secs,
            warn_after_days,
        })
    }

    /// The bytes a guest reads.
    pub fn pem(&self) -> &[u8] {
        &self.pem
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn source(&self) -> &TrustSource {
        &self.source
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    /// The modification time guests see on the file, in nanoseconds since
    /// the Unix epoch. It is derived from the content, not from any clock
    /// or file: [`MODIFIED_EPOCH_UNIX_SECS`] plus the first eight bytes of
    /// the SHA-256, read big-endian, as seconds modulo
    /// [`MODIFIED_SPAN_SECS`]. Two nodes serving the same bytes agree, and a
    /// rollback changes it, but it says nothing about when the bundle was
    /// published.
    pub fn modified_ns(&self) -> u64 {
        modified_ns(&self.sha256)
    }

    pub fn facts(&self) -> TrustFacts {
        TrustFacts {
            source: match &self.source {
                TrustSource::Shipped => "shipped".to_string(),
                TrustSource::Operator(path) => path.display().to_string(),
            },
            bundle_date: self.manifest.as_ref().map(|m| m.bundle_date.clone()),
            generation: self.manifest.as_ref().map(|m| m.generation),
            sha256: hex::encode(self.sha256),
            certificate_count: self.certificate_count,
            ingested_at: self.manifest.as_ref().map(|m| m.ingested_at.clone()),
        }
    }

    /// How old the last upstream check is at `now`. A check in the future
    /// counts as zero days old.
    pub fn staleness(&self, now: SystemTime) -> Staleness {
        let (Some(last), Some(manifest)) = (self.last_check_unix_secs, &self.manifest) else {
            return Staleness::Unknown;
        };
        let now_secs = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let age_days = now_secs.saturating_sub(last) / SECS_PER_DAY;
        if age_days > u64::from(self.warn_after_days) {
            Staleness::Stale {
                age_days,
                warn_after_days: self.warn_after_days,
                last_check: manifest.ingested_at.clone(),
                bundle_date: manifest.bundle_date.clone(),
            }
        } else {
            Staleness::Fresh { age_days }
        }
    }
}

/// See [`TrustBundle::modified_ns`].
pub fn modified_ns(sha256: &[u8; 32]) -> u64 {
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

fn count_certificates(pem: &[u8]) -> usize {
    pem.windows(PEM_BEGIN.len())
        .filter(|window| *window == PEM_BEGIN)
        .count()
}

/// `YYYY-MM-DDTHH:MM:SSZ` to seconds since the Unix epoch.
fn parse_utc(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let time = time.strip_suffix('Z')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    if date.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut time = time.split(':');
    let hour: u64 = time.next()?.parse().ok()?;
    let minute: u64 = time.next()?.parse().ok()?;
    let second: u64 = time.next()?.parse().ok()?;
    if time.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let days = u64::try_from(days).ok()?;
    Some(days * SECS_PER_DAY + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_from_march = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const INGESTED_AT_2026_09_11: u64 = 1_789_105_175;

    fn at(unix_secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix_secs)
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
            warn_after_days: 30,
        }
    }

    #[test]
    fn the_shipped_bundle_matches_its_manifest() {
        let bundle = TrustBundle::load(&TrustConfig::default()).expect("the shipped bundle loads");
        let manifest = shipped_manifest();
        let facts = bundle.facts();
        assert_eq!(
            facts.sha256, manifest.bundle_sha256,
            "the embedded bytes must be the ones the manifest was written for"
        );
        assert_eq!(
            facts.certificate_count, manifest.certificate_count as usize,
            "the manifest's count must describe the embedded bundle"
        );
        assert_eq!(
            facts.generation,
            Some(manifest.generation),
            "the facts carry the manifest's generation"
        );
        assert_eq!(
            facts.source, "shipped",
            "a node without trust.bundle serves the shipped bundle"
        );
    }

    #[test]
    fn the_manifest_parser_reads_every_field_the_ingest_script_writes() {
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
        assert_eq!(manifest.generation, 3, "generation is read as an integer");
        assert_eq!(
            manifest.certificate_count, 121,
            "the certificate count is read as an integer"
        );
        assert_eq!(
            manifest.ingested_at, "2026-09-11T05:39:35Z",
            "ingested_at is read verbatim"
        );
    }

    #[test]
    fn a_manifest_with_a_field_the_daemon_does_not_read_is_refused() {
        let err = parse_manifest("m", r#"{"generation": 1, "extra": true}"#)
            .expect_err("an unknown field must not load silently");
        assert!(
            matches!(&err, TrustError::ManifestMalformed { path, error } if path == "m" && error.contains("extra")),
            "the refusal must name the field: {err}"
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
            warn_after_days: 30,
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
            warn_after_days: 30,
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
            warn_after_days: 30,
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
            bundle.facts().generation,
            Some(manifest.generation),
            "the sibling manifest supplies the generation"
        );
        assert_eq!(
            bundle.source(),
            &TrustSource::Operator(path),
            "the facts say whose bundle this is"
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
    fn an_unreadable_bundle_names_its_path() {
        let cfg = TrustConfig {
            bundle: Some(PathBuf::from("/nonexistent/roots.pem")),
            sha256: Some("0".repeat(64)),
            warn_after_days: 30,
        };
        let err = TrustBundle::load(&cfg).expect_err("a missing file cannot be served");
        assert!(
            matches!(&err, TrustError::Unreadable { path, .. } if path == Path::new("/nonexistent/roots.pem")),
            "the refusal names the file: {err}"
        );
    }

    #[test]
    fn a_manifest_with_an_unparseable_check_time_is_refused() {
        let mut manifest = shipped_manifest();
        manifest.ingested_at = "yesterday".into();
        let (_dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let err = TrustBundle::load(&operator_config(&path, &manifest.bundle_sha256))
            .expect_err("an age that cannot be computed must not pass as fresh");
        assert!(
            matches!(&err, TrustError::IngestedAtMalformed { value, .. } if value == "yesterday"),
            "the refusal quotes the value: {err}"
        );
    }

    fn bundle_checked_at(ingested_at: &str, warn_after_days: u32) -> TrustBundle {
        let mut manifest = shipped_manifest();
        manifest.ingested_at = ingested_at.into();
        let (dir, path) = operator_dir(SHIPPED_PEM, Some(&manifest));
        let mut cfg = operator_config(&path, &manifest.bundle_sha256);
        cfg.warn_after_days = warn_after_days;
        let bundle = TrustBundle::load(&cfg).expect("a dated operator bundle loads");
        drop(dir);
        bundle
    }

    #[test]
    fn a_check_within_the_window_is_fresh() {
        let bundle = bundle_checked_at("2026-09-11T05:39:35Z", 30);
        let now = at(INGESTED_AT_2026_09_11 + 30 * SECS_PER_DAY);
        assert_eq!(
            bundle.staleness(now),
            Staleness::Fresh { age_days: 30 },
            "exactly warn_after_days old is still inside the window"
        );
    }

    #[test]
    fn a_check_older_than_the_window_warns_with_both_dates() {
        let bundle = bundle_checked_at("2026-09-11T05:39:35Z", 30);
        let now = at(INGESTED_AT_2026_09_11 + 31 * SECS_PER_DAY);
        let staleness = bundle.staleness(now);
        assert_eq!(
            staleness,
            Staleness::Stale {
                age_days: 31,
                warn_after_days: 30,
                last_check: "2026-09-11T05:39:35Z".into(),
                bundle_date: shipped_manifest().bundle_date,
            },
            "one day past the window is stale"
        );
        let message = staleness.to_string();
        assert!(
            message.contains("2026-09-11T05:39:35Z") && message.contains("publication date"),
            "the warning distinguishes the check date from the bundle's date: {message}"
        );
    }

    #[test]
    fn a_check_in_the_future_is_not_stale() {
        let bundle = bundle_checked_at("2026-09-11T05:39:35Z", 30);
        assert_eq!(
            bundle.staleness(at(INGESTED_AT_2026_09_11 - SECS_PER_DAY)),
            Staleness::Fresh { age_days: 0 },
            "a clock behind the ingest must not warn"
        );
    }

    #[test]
    fn an_operator_bundle_without_a_manifest_has_unknown_age() {
        let (_dir, path) = operator_dir(SHIPPED_PEM, None);
        let bundle = TrustBundle::load(&operator_config(&path, &shipped_manifest().bundle_sha256))
            .expect("an operator bundle without a manifest loads");
        assert_eq!(
            bundle.staleness(at(INGESTED_AT_2026_09_11)),
            Staleness::Unknown,
            "with no check on record the node says so rather than guessing"
        );
        assert_eq!(
            bundle.facts().ingested_at,
            None,
            "the facts carry no check date either"
        );
    }

    #[test]
    fn the_guest_timestamp_is_derived_from_the_content() {
        let a: [u8; 32] = Sha256::digest(b"bundle a").into();
        let b: [u8; 32] = Sha256::digest(b"bundle b").into();
        assert_eq!(
            modified_ns(&a),
            modified_ns(&a),
            "the same bytes give the same timestamp on every node"
        );
        assert_ne!(
            modified_ns(&a),
            modified_ns(&b),
            "different bytes give a different timestamp, so a rollback is visible"
        );
        let secs = modified_ns(&a) / 1_000_000_000;
        assert!(
            (MODIFIED_EPOCH_UNIX_SECS..MODIFIED_EPOCH_UNIX_SECS + MODIFIED_SPAN_SECS)
                .contains(&secs),
            "the timestamp lies within fifty years of 2000-01-01"
        );
        assert_eq!(
            modified_ns(&a) % 1_000_000_000,
            0,
            "the derivation is in whole seconds"
        );
    }

    #[test]
    fn utc_timestamps_parse_to_unix_seconds() {
        assert_eq!(
            parse_utc("2026-09-11T05:39:35Z"),
            Some(INGESTED_AT_2026_09_11),
            "a date after a leap year and a century boundary"
        );
        assert_eq!(
            parse_utc("2000-01-01T00:00:00Z"),
            Some(MODIFIED_EPOCH_UNIX_SECS),
            "the guest timestamp epoch"
        );
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0), "the Unix epoch");
        for bad in [
            "2026-09-11T05:39:35",
            "2026-09-11 05:39:35Z",
            "2026-13-01T00:00:00Z",
            "2026-09-11T24:00:00Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert_eq!(parse_utc(bad), None, "`{bad}` is not accepted");
        }
    }

    #[test]
    fn certificates_are_counted_by_their_begin_line() {
        let pem = b"junk\n-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n\
                    -----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n";
        assert_eq!(count_certificates(pem), 2, "two blocks, two certificates");
        assert_eq!(count_certificates(b""), 0, "no blocks, no certificates");
    }
}
