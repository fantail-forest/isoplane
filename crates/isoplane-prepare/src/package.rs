//! Application packages as `isoplane bundle` writes them, mapped to the files
//! the host serves the shared runtime: `application.json` and `assemblies/`
//! beneath the package root, `assets/` at the application's content root.
//! Its `runtime.json` names the runtime it was built against and is not served.

use isoplane_abi::filesystem::{APPLICATION_PACKAGE_ROOT, HOST_ROOT};
use isoplane_engine::{PlatformFile, PlatformFiles, PlatformFilesError};

const MANIFEST: &str = "application.json";
const ASSEMBLIES: &str = "assemblies/";
const ASSETS: &str = "assets/";
const RUNTIME: &str = "runtime.json";
/// The production runtime module the SDK carries, as its `runtime.json` names it.
pub const RUNTIME_MODULE: &str = "Isoplane.MonoRuntime.wasm";
/// A runtime identity is a few hundred bytes; this bounds reading a hostile one.
const RUNTIME_IDENTITY_BYTES: u64 = 64 * 1024;

/// Why a package archive is refused.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// The archive is not a readable tar.
    #[error("reading the package archive: {0}")]
    Archive(#[source] std::io::Error),
    /// Links and devices have no meaning in a package.
    #[error("package entry `{path}` is not a regular file")]
    NotAFile { path: String },
    /// A path that could name the same file two ways, or escape its directory.
    #[error("package entry `{path}` is not a normalised relative path")]
    Path { path: String },
    /// An entry the bundle format does not define.
    #[error("package entry `{path}` is outside {MANIFEST}, {ASSEMBLIES} and {ASSETS}")]
    Unexpected { path: String },
    /// An asset that would be served where the host serves its own files.
    #[error("package asset `{path}` would be served beneath the host's {HOST_ROOT}")]
    ReservedAsset { path: String },
    /// The package names no entry assembly or factory.
    #[error("the package has no {MANIFEST}")]
    NoManifest,
    /// The package names no runtime to run on.
    #[error("the package has no {RUNTIME}; build it with an SDK that carries its shared runtime")]
    NoRuntime,
    /// The package's files add up to more than its size limit.
    #[error(
        "the package holds more than {limit_bytes} bytes uncompressed, its application size limit"
    )]
    TooLarge { limit_bytes: u64 },
    /// The package holds more files than its limit.
    #[error("the package holds more than {limit} files")]
    TooManyFiles { limit: u32 },
    /// The package's manifest is larger than its limit.
    #[error("the package's {MANIFEST} is larger than {limit_bytes} bytes")]
    ManifestTooLarge { limit_bytes: u64 },
    /// The package's runtime identity is unreadable or ambiguous.
    #[error("the package's {RUNTIME} is refused: {0}")]
    Runtime(#[from] RuntimeIdentityError),
    /// Two entries serve the same path, or a file where another needs a directory.
    #[error(transparent)]
    Files(#[from] PlatformFilesError),
}

/// A package as the host prepares it.
#[derive(Debug)]
pub struct Package {
    /// The files the host serves while preparing and running it.
    pub files: PlatformFiles,
    /// The runtime it was built against.
    pub runtime: RuntimeIdentity,
}

/// What a package may hold.
#[derive(Clone, Copy, Debug)]
pub struct PackageLimits {
    /// Uncompressed bytes of every file it holds.
    pub bytes: u64,
    /// Files it holds.
    pub files: u32,
    /// Bytes of its `application.json`.
    pub manifest_bytes: u64,
}

impl PackageLimits {
    /// The limits `manifest`'s application is held to.
    pub fn of(manifest: &isoplane_abi::config::AppManifest) -> Self {
        Self {
            bytes: manifest.effective_limits().package_bytes,
            files: manifest.validation_limits.max_package_files,
            manifest_bytes: manifest.validation_limits.max_package_manifest_bytes,
        }
    }
}

/// Reads a package archive, refusing it as soon as it passes one of
/// `limits`, before reading or recording the entry that passes it.
pub fn read(archive: &[u8], limits: PackageLimits) -> Result<Package, PackageError> {
    let mut files = Vec::new();
    let mut runtime = None;
    let mut total_bytes = 0u64;
    let mut count = 0u32;
    let mut entries = tar::Archive::new(archive);
    for entry in entries.entries().map_err(PackageError::Archive)? {
        let mut entry = entry.map_err(PackageError::Archive)?;
        let path = String::from_utf8(entry.path_bytes().into_owned()).map_err(|error| {
            PackageError::Path {
                path: String::from_utf8_lossy(error.as_bytes()).into_owned(),
            }
        })?;
        if !entry.header().entry_type().is_file() {
            return Err(PackageError::NotAFile { path });
        }
        count = count.saturating_add(1);
        if count > limits.files {
            return Err(PackageError::TooManyFiles {
                limit: limits.files,
            });
        }
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > limits.bytes {
            return Err(PackageError::TooLarge {
                limit_bytes: limits.bytes,
            });
        }
        if path == MANIFEST && entry.size() > limits.manifest_bytes {
            return Err(PackageError::ManifestTooLarge {
                limit_bytes: limits.manifest_bytes,
            });
        }
        if path == RUNTIME {
            if runtime.is_some() || entry.size() > RUNTIME_IDENTITY_BYTES {
                return Err(RuntimeIdentityError::Ambiguous.into());
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).map_err(PackageError::Archive)?;
            runtime = Some(RuntimeIdentity::from_json(&bytes)?);
            continue;
        }
        let served = served_path(&path)?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes).map_err(PackageError::Archive)?;
        files.push(served_file(served, bytes));
    }
    if !files
        .iter()
        .any(|file| file.path == format!("{APPLICATION_PACKAGE_ROOT}/{MANIFEST}"))
    {
        return Err(PackageError::NoManifest);
    }
    Ok(Package {
        files: PlatformFiles::new(files)?,
        runtime: runtime.ok_or(PackageError::NoRuntime)?,
    })
}

/// The shared runtime a package was built against, as the `runtime.json`
/// its SDK carries names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeIdentity {
    /// The SDK release that carries the runtime.
    pub sdk_version: String,
    /// SHA-256 of [`RUNTIME_MODULE`].
    pub module_sha256: [u8; 32],
}

/// Why a runtime identity is refused.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeIdentityError {
    /// Not the SDK's `runtime.json` shape.
    #[error("not the SDK's runtime.json shape: {0}")]
    Json(#[source] serde_json::Error),
    /// The version could not name a directory or a release unambiguously.
    #[error("`{0}` is not an SDK release version")]
    Version(String),
    /// The production runtime module is missing or its digest malformed.
    #[error("it records no SHA-256 of {RUNTIME_MODULE}")]
    Module,
    /// More than one identity, or one too large to be genuine.
    #[error("a package carries exactly one small runtime identity")]
    Ambiguous,
}

impl RuntimeIdentity {
    /// Reads the identity a `runtime.json` records, ignoring the provenance
    /// it also carries.
    pub fn from_json(bytes: &[u8]) -> Result<Self, RuntimeIdentityError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Recorded {
            sdk_version: String,
            modules: std::collections::BTreeMap<String, Module>,
        }
        #[derive(serde::Deserialize)]
        struct Module {
            sha256: String,
        }
        let recorded: Recorded =
            serde_json::from_slice(bytes).map_err(RuntimeIdentityError::Json)?;
        let release = !recorded.sdk_version.is_empty()
            && recorded.sdk_version.len() <= 128
            && recorded
                .sdk_version
                .starts_with(|c: char| c.is_ascii_digit())
            && recorded
                .sdk_version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
        if !release {
            return Err(RuntimeIdentityError::Version(recorded.sdk_version));
        }
        let digest = recorded
            .modules
            .get(RUNTIME_MODULE)
            .filter(|module| {
                module
                    .sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            .and_then(|module| hex::decode(&module.sha256).ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .ok_or(RuntimeIdentityError::Module)?;
        Ok(Self {
            sdk_version: recorded.sdk_version,
            module_sha256: digest,
        })
    }
}

fn served_path(path: &str) -> Result<String, PackageError> {
    let normalised = !path.is_empty()
        && !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."));
    if !normalised {
        return Err(PackageError::Path { path: path.into() });
    }
    if path == MANIFEST || path.starts_with(ASSEMBLIES) {
        return Ok(format!("{APPLICATION_PACKAGE_ROOT}/{path}"));
    }
    let Some(asset) = path.strip_prefix(ASSETS) else {
        return Err(PackageError::Unexpected { path: path.into() });
    };
    let served = format!("/{asset}");
    if served == HOST_ROOT || served.starts_with(&format!("{HOST_ROOT}/")) {
        return Err(PackageError::ReservedAsset { path: path.into() });
    }
    Ok(served)
}

/// Stamps a file with a time derived from its content, so its metadata, and
/// with it the template's identity, depends only on what the package holds.
fn served_file(path: String, bytes: Vec<u8>) -> PlatformFile {
    let digest = blake3::hash(&bytes);
    let mut stamp = [0; 8];
    stamp.copy_from_slice(&digest.as_bytes()[..8]);
    PlatformFile {
        path,
        bytes: bytes.into(),
        modified_ns: u64::from_le_bytes(stamp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &[u8] = br#"{"sdkVersion":"1.2.3-test","sourceRevision":"abc","modules":{"Isoplane.MonoRuntime.wasm":{"sha256":"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"}}}"#;

    /// A package with `entries` built against the runtime [`IDENTITY`] names.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut all = vec![("runtime.json", IDENTITY)];
        all.extend_from_slice(entries);
        entries_only(&all)
    }

    fn entries_only(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder
                .append_data(&mut header, path, *bytes)
                .expect("append a test entry");
        }
        builder.into_inner().expect("finish the test archive")
    }

    /// Writes `path` into the header's name field unchecked, as a hostile
    /// archive would, since the builder refuses to.
    fn hostile(path: &str) -> Vec<u8> {
        let mut header = tar::Header::new_ustar();
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(1);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&[b'x'; 512]);
        bytes.extend_from_slice(&[0; 1024]);
        bytes
    }

    fn application_files(archive: &[u8]) -> Result<PlatformFiles, PackageError> {
        read(archive, UNBOUNDED).map(|package| package.files)
    }

    fn paths(files: &PlatformFiles) -> Vec<&str> {
        files
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect()
    }

    const UNBOUNDED: PackageLimits = PackageLimits {
        bytes: u64::MAX,
        files: u32::MAX,
        manifest_bytes: u64::MAX,
    };

    #[test]
    fn a_package_is_measured_by_the_uncompressed_bytes_of_every_file_it_holds() {
        let package = archive(&[
            ("application.json", b"{}"),
            ("assemblies/App.dll", &[0; 100]),
        ]);
        let size = (IDENTITY.len() + 2 + 100) as u64;
        let limited = |bytes| PackageLimits { bytes, ..UNBOUNDED };
        assert!(
            read(&package, limited(size)).is_ok(),
            "a package exactly at its limit must be admitted"
        );
        assert!(
            matches!(read(&package, limited(size - 1)), Err(PackageError::TooLarge { limit_bytes }) if limit_bytes == size - 1),
            "a package one byte over its limit must be refused"
        );
    }

    #[test]
    fn a_package_of_many_empty_files_is_refused_at_its_file_limit() {
        let names: Vec<String> = (0..64).map(|index| format!("assets/{index}")).collect();
        let mut entries: Vec<(&str, &[u8])> = vec![("application.json", b"{}")];
        entries.extend(names.iter().map(|name| (name.as_str(), b"".as_slice())));
        let package = archive(&entries);
        let limited = |files| PackageLimits { files, ..UNBOUNDED };
        assert!(
            read(&package, limited(66)).is_ok(),
            "a package exactly at its file limit must be admitted"
        );
        assert!(
            matches!(
                read(&package, limited(65)),
                Err(PackageError::TooManyFiles { limit: 65 })
            ),
            "files that cost no bytes must still count against the file limit"
        );
    }

    #[test]
    fn a_package_manifest_over_its_limit_is_refused() {
        let manifest = [b' '; 100];
        let package = archive(&[("application.json", &manifest)]);
        let limited = |manifest_bytes| PackageLimits {
            manifest_bytes,
            ..UNBOUNDED
        };
        assert!(
            read(&package, limited(100)).is_ok(),
            "a manifest exactly at its limit must be admitted"
        );
        assert!(
            matches!(
                read(&package, limited(99)),
                Err(PackageError::ManifestTooLarge { limit_bytes: 99 })
            ),
            "a manifest over its limit must be refused before it is read"
        );
    }

    #[test]
    fn a_package_serves_its_manifest_and_assemblies_beneath_the_package_root_and_assets_at_the_content_root(
    ) {
        let files = application_files(&archive(&[
            ("application.json", b"{}"),
            ("assemblies/App.dll", b"il"),
            ("assemblies/de/App.resources.dll", b"il"),
            ("assets/wwwroot/site.css", b"css"),
        ]))
        .expect("a well-formed package maps");
        let mut served = paths(&files);
        served.sort_unstable();
        assert_eq!(
            served,
            [
                "/.isoplane/package/application.json",
                "/.isoplane/package/assemblies/App.dll",
                "/.isoplane/package/assemblies/de/App.resources.dll",
                "/wwwroot/site.css",
            ],
            "the runtime's loader and the application must find each entry where the host serves it"
        );
    }

    #[test]
    fn a_package_without_a_manifest_is_refused() {
        assert!(
            matches!(
                application_files(&archive(&[("assemblies/App.dll", b"il")])),
                Err(PackageError::NoManifest)
            ),
            "a package must name its entry assembly and factory"
        );
    }

    #[test]
    fn an_asset_beneath_the_host_root_is_refused() {
        for asset in [
            "assets/.isoplane/package/application.json",
            "assets/.isoplane",
        ] {
            assert!(
                matches!(
                    application_files(&archive(&[("application.json", b"{}"), (asset, b"x")])),
                    Err(PackageError::ReservedAsset { .. })
                ),
                "an asset must not shadow what the host serves: {asset}"
            );
        }
    }

    #[test]
    fn an_entry_the_bundle_format_does_not_define_is_refused() {
        assert!(
            matches!(
                application_files(&archive(&[("application.json", b"{}"), ("App.dll", b"il")])),
                Err(PackageError::Unexpected { .. })
            ),
            "only the manifest, assemblies and assets may be served"
        );
    }

    #[test]
    fn an_ambiguous_or_escaping_path_is_refused() {
        for path in [
            "assets/../escape",
            "assets/./a",
            "assets//a",
            "/assets/a",
            "assets/a\\b",
        ] {
            assert!(
                matches!(
                    application_files(&hostile(path)),
                    Err(PackageError::Path { .. })
                ),
                "an entry must name one file inside its directory: {path:?}"
            );
        }
    }

    #[test]
    fn a_link_is_refused() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "assets/passwd", "/etc/passwd")
            .expect("append a link");
        let bytes = builder.into_inner().expect("finish the archive");
        assert!(
            matches!(
                application_files(&bytes),
                Err(PackageError::NotAFile { .. })
            ),
            "a link must not stand in for content"
        );
    }

    #[test]
    fn two_entries_serving_one_path_are_refused() {
        assert!(
            matches!(
                application_files(&archive(&[
                    ("application.json", b"{}"),
                    ("assets/a", b"1"),
                    ("assets/a", b"2"),
                ])),
                Err(PackageError::Files(
                    PlatformFilesError::DuplicatePath { .. }
                ))
            ),
            "a later entry must not silently replace an earlier one"
        );
    }

    #[test]
    fn a_package_names_the_runtime_it_was_built_against_and_does_not_serve_it() {
        let package =
            read(&archive(&[("application.json", b"{}")]), UNBOUNDED).expect("a package maps");
        assert_eq!(
            (
                package.runtime.sdk_version.as_str(),
                package.runtime.module_sha256[..4].to_vec()
            ),
            ("1.2.3-test", vec![0x00, 0x11, 0x22, 0x33]),
            "the host must run a package on the runtime its runtime.json names"
        );
        assert_eq!(
            paths(&package.files),
            ["/.isoplane/package/application.json"],
            "the runtime identity is the platform's to read, not the guest's"
        );
    }

    #[test]
    fn a_package_without_a_runtime_identity_is_refused() {
        assert!(
            matches!(
                read(&entries_only(&[("application.json", b"{}")]), UNBOUNDED),
                Err(PackageError::NoRuntime)
            ),
            "a package the host cannot match to a runtime must not be prepared"
        );
    }

    #[test]
    fn an_unreadable_or_repeated_runtime_identity_is_refused() {
        let digest =
            "\"sha256\":\"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\"";
        let path_version = format!("{{\"sdkVersion\":\"../1\",\"modules\":{{\"Isoplane.MonoRuntime.wasm\":{{{digest}}}}}}}");
        let other_module =
            format!("{{\"sdkVersion\":\"1.0\",\"modules\":{{\"Other.wasm\":{{{digest}}}}}}}");
        let short_digest =
            r#"{"sdkVersion":"1.0","modules":{"Isoplane.MonoRuntime.wasm":{"sha256":"0011"}}}"#;
        for (identity, expected) in [
            (b"not json".as_slice(), "Json"),
            (path_version.as_bytes(), "Version"),
            (other_module.as_bytes(), "Module"),
            (short_digest.as_bytes(), "Module"),
        ] {
            let refused = read(
                &entries_only(&[("runtime.json", identity), ("application.json", b"{}")]),
                UNBOUNDED,
            );
            let variant = match refused {
                Err(PackageError::Runtime(RuntimeIdentityError::Json(_))) => "Json",
                Err(PackageError::Runtime(RuntimeIdentityError::Version(_))) => "Version",
                Err(PackageError::Runtime(RuntimeIdentityError::Module)) => "Module",
                _ => "accepted or another refusal",
            };
            assert_eq!(
                variant,
                expected,
                "a runtime identity that names no single runtime must be refused: {}",
                String::from_utf8_lossy(identity)
            );
        }
        assert!(
            matches!(
                read(
                    &archive(&[("runtime.json", IDENTITY), ("application.json", b"{}")]),
                    UNBOUNDED
                ),
                Err(PackageError::Runtime(RuntimeIdentityError::Ambiguous))
            ),
            "a package must name exactly one runtime"
        );
    }

    #[test]
    fn a_files_time_depends_only_on_its_content() {
        let first = application_files(&archive(&[("application.json", b"{}")]))
            .expect("a manifest-only package maps");
        let second = application_files(&archive(&[("application.json", b"{}")]))
            .expect("a manifest-only package maps");
        assert_eq!(
            first.files()[0].modified_ns,
            second.files()[0].modified_ns,
            "rebuilding identical content must not change the template's identity"
        );
    }
}
