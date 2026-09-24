//! Application packages as `isoplane bundle` writes them, mapped to the files
//! the host serves the shared runtime: `application.json` and `assemblies/`
//! beneath the package root, `assets/` at the application's content root.

use isoplane_abi::filesystem::{APPLICATION_PACKAGE_ROOT, HOST_ROOT};
use isoplane_engine::{PlatformFile, PlatformFiles, PlatformFilesError};

const MANIFEST: &str = "application.json";
const ASSEMBLIES: &str = "assemblies/";
const ASSETS: &str = "assets/";

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
    /// Two entries serve the same path, or a file where another needs a directory.
    #[error(transparent)]
    Files(#[from] PlatformFilesError),
}

/// Maps a package archive to the application files the host serves while
/// preparing and running it on the shared runtime.
pub fn application_files(archive: &[u8]) -> Result<PlatformFiles, PackageError> {
    let mut files = Vec::new();
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
    Ok(PlatformFiles::new(files)?)
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

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    fn paths(files: &PlatformFiles) -> Vec<&str> {
        files
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect()
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
