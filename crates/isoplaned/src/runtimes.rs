//! The shared runtimes a node installation supports: `inventory.json` lists
//! the SDK releases, and each release's directory holds the `runtime.json`
//! that release's SDK carries and the runtime module it names.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoplane_prepare::package::{RuntimeIdentity, RUNTIME_MODULE};
use sha2::Digest;

const INVENTORY: &str = "inventory.json";
const IDENTITY: &str = "runtime.json";

/// The runtimes installed with this node, checked when it starts.
pub struct Runtimes {
    installed: BTreeMap<String, Installed>,
}

struct Installed {
    identity: RuntimeIdentity,
    module: PathBuf,
}

/// A package resolved to the installed runtime it was built against.
pub struct PackageOnRuntime {
    /// The SDK release the package was built with.
    pub sdk_version: String,
    /// The runtime module, checked against its identity.
    pub runtime: Vec<u8>,
    /// The provenance a template prepared from it records.
    pub profile: isoplane_prepare::RuntimeProfile,
    /// The files the package serves.
    pub files: isoplane_engine::PlatformFiles,
}

/// Why a package is not prepared on any runtime this node installs.
#[derive(Debug, thiserror::Error)]
pub enum PackageRefusal {
    /// The package itself is malformed.
    #[error(transparent)]
    Package(#[from] isoplane_prepare::package::PackageError),
    /// No runtime is installed for the package's SDK release.
    #[error("the package was built with SDK {requested}, which this platform does not support; supported SDK releases: {supported}")]
    Unsupported {
        requested: String,
        supported: String,
    },
    /// The package names a runtime other than the one installed for its release.
    #[error("the package was built against a runtime for SDK {sdk_version} other than the one released with it; rebuild it with the released SDK")]
    Mismatched { sdk_version: String },
    /// The installed runtime changed after the node checked it.
    #[error("the installed runtime for SDK {sdk_version} no longer matches its identity")]
    Changed { sdk_version: String },
    /// The installed runtime could not be read.
    #[error("reading the installed runtime for SDK {sdk_version}: {source}")]
    Read {
        sdk_version: String,
        #[source]
        source: std::io::Error,
    },
}

impl Runtimes {
    /// Reads the inventory in `directory` and checks every runtime it lists
    /// against its identity.
    pub fn load(directory: &Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Inventory {
            sdk_versions: Vec<String>,
        }
        let path = directory.join(INVENTORY);
        let inventory: Inventory = serde_json::from_slice(
            &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
        )
        .with_context(|| format!("{} is not a runtime inventory", path.display()))?;
        anyhow::ensure!(
            !inventory.sdk_versions.is_empty(),
            "{} lists no supported SDK release",
            path.display()
        );
        let mut installed = BTreeMap::new();
        for version in inventory.sdk_versions {
            let release = directory.join(&version);
            let identity_path = release.join(IDENTITY);
            let identity = RuntimeIdentity::from_json(
                &std::fs::read(&identity_path)
                    .with_context(|| format!("reading {}", identity_path.display()))?,
            )
            .with_context(|| format!("{} is refused", identity_path.display()))?;
            anyhow::ensure!(
                identity.sdk_version == version,
                "{} names SDK {}, not the {version} its inventory lists",
                identity_path.display(),
                identity.sdk_version
            );
            let entry = Installed {
                identity,
                module: release.join(RUNTIME_MODULE),
            };
            entry.read()?;
            anyhow::ensure!(
                installed.insert(version.clone(), entry).is_none(),
                "{} lists SDK {version} twice",
                path.display()
            );
        }
        Ok(Self { installed })
    }

    /// The supported SDK releases, for the node's startup record.
    pub fn sdk_versions(&self) -> impl Iterator<Item = &str> {
        self.installed.keys().map(String::as_str)
    }

    /// Whether a runtime is installed for SDK `sdk_version`.
    pub fn supports(&self, sdk_version: &str) -> bool {
        self.installed.contains_key(sdk_version)
    }

    /// Reads `package` within `limits` and resolves the installed runtime it names.
    pub fn resolve(
        &self,
        package: &[u8],
        limits: isoplane_prepare::package::PackageLimits,
    ) -> Result<PackageOnRuntime, PackageRefusal> {
        let package = isoplane_prepare::package::read(package, limits)?;
        Ok(PackageOnRuntime {
            runtime: self.module(&package.runtime)?,
            profile: isoplane_prepare::RuntimeProfile::SharedRuntime {
                sdk_version: package.runtime.sdk_version.clone(),
            },
            sdk_version: package.runtime.sdk_version,
            files: package.files,
        })
    }

    fn module(&self, identity: &RuntimeIdentity) -> Result<Vec<u8>, PackageRefusal> {
        let installed = self.installed.get(&identity.sdk_version).ok_or_else(|| {
            PackageRefusal::Unsupported {
                requested: identity.sdk_version.clone(),
                supported: self.sdk_versions().collect::<Vec<_>>().join(", "),
            }
        })?;
        if installed.identity != *identity {
            return Err(PackageRefusal::Mismatched {
                sdk_version: identity.sdk_version.clone(),
            });
        }
        installed.read()
    }
}

impl Installed {
    fn read(&self) -> Result<Vec<u8>, PackageRefusal> {
        let sdk_version = &self.identity.sdk_version;
        let bytes = std::fs::read(&self.module).map_err(|source| PackageRefusal::Read {
            sdk_version: sdk_version.clone(),
            source,
        })?;
        if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != self.identity.module_sha256 {
            return Err(PackageRefusal::Changed {
                sdk_version: sdk_version.clone(),
            });
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE: &[u8] = b"\0asm\x01\0\0\0";

    fn identity_json(version: &str, module: &[u8]) -> String {
        format!(
            r#"{{"sdkVersion":"{version}","sourceRevision":"test","modules":{{"{RUNTIME_MODULE}":{{"sha256":"{}"}}}}}}"#,
            hex::encode(sha2::Sha256::digest(module))
        )
    }

    /// An installation supporting `versions`, each with [`MODULE`].
    fn installation(versions: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("installation");
        for version in versions {
            let release = root.path().join(version);
            std::fs::create_dir(&release).expect("release directory");
            std::fs::write(release.join(RUNTIME_MODULE), MODULE).expect("module");
            std::fs::write(release.join(IDENTITY), identity_json(version, MODULE))
                .expect("identity");
        }
        std::fs::write(
            root.path().join(INVENTORY),
            serde_json::json!({ "sdkVersions": versions }).to_string(),
        )
        .expect("inventory");
        root
    }

    fn identity(version: &str, module: &[u8]) -> RuntimeIdentity {
        RuntimeIdentity::from_json(identity_json(version, module).as_bytes()).expect("identity")
    }

    #[test]
    fn a_package_resolves_to_the_runtime_released_with_its_sdk() {
        let root = installation(&["1.0.0", "1.1.0"]);
        let runtimes = Runtimes::load(root.path()).expect("a checked installation loads");
        assert_eq!(
            runtimes
                .module(&identity("1.1.0", MODULE))
                .expect("resolves"),
            MODULE,
            "a package must run on the runtime its SDK release installed"
        );
    }

    #[test]
    fn a_package_for_an_unsupported_release_is_refused_naming_the_supported_ones() {
        let root = installation(&["1.0.0"]);
        let runtimes = Runtimes::load(root.path()).expect("loads");
        let refusal = runtimes.module(&identity("2.0.0", MODULE));
        assert!(
            matches!(&refusal, Err(PackageRefusal::Unsupported { supported, .. }) if supported == "1.0.0"),
            "a customer must learn which SDK releases the platform runs: {refusal:?}"
        );
    }

    #[test]
    fn a_package_built_against_another_runtime_for_its_release_is_refused() {
        let root = installation(&["1.0.0"]);
        let runtimes = Runtimes::load(root.path()).expect("loads");
        assert!(
            matches!(
                runtimes.module(&identity("1.0.0", b"another build")),
                Err(PackageRefusal::Mismatched { .. })
            ),
            "a package must not run on a runtime other than the one it was built against"
        );
    }

    #[test]
    fn an_installed_runtime_changed_after_startup_is_refused() {
        let root = installation(&["1.0.0"]);
        let runtimes = Runtimes::load(root.path()).expect("loads");
        std::fs::write(root.path().join("1.0.0").join(RUNTIME_MODULE), b"changed")
            .expect("replace the module");
        assert!(
            matches!(
                runtimes.module(&identity("1.0.0", MODULE)),
                Err(PackageRefusal::Changed { .. })
            ),
            "only the runtime the installation recorded may be prepared"
        );
    }

    #[test]
    fn an_installation_whose_runtime_differs_from_its_identity_does_not_load() {
        let root = installation(&["1.0.0"]);
        std::fs::write(root.path().join("1.0.0").join(RUNTIME_MODULE), b"changed")
            .expect("replace the module");
        assert!(
            Runtimes::load(root.path()).is_err(),
            "a node must not start on runtimes that differ from their identity"
        );
    }

    #[test]
    fn an_inventory_listing_a_release_under_another_name_does_not_load() {
        let root = installation(&["1.0.0"]);
        std::fs::write(
            root.path().join("1.0.0").join(IDENTITY),
            identity_json("1.0.1", MODULE),
        )
        .expect("mislabel the release");
        assert!(
            Runtimes::load(root.path()).is_err(),
            "each listed release must hold the runtime released with it"
        );
    }

    #[test]
    fn a_package_of_many_empty_files_is_refused_before_its_runtime_is_resolved() {
        let root = installation(&["1.0.0"]);
        let runtimes = Runtimes::load(root.path()).expect("loads");
        let mut builder = tar::Builder::new(Vec::new());
        for index in 0..8 {
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder
                .append_data(&mut header, format!("assets/{index}"), std::io::empty())
                .expect("entry");
        }
        let archive = builder.into_inner().expect("archive");
        let mut manifest: isoplane_abi::config::AppManifest =
            toml::from_str("name = \"app\"").expect("a minimal manifest");
        manifest.validation_limits.max_package_files = 4;
        assert!(
            matches!(
                runtimes.resolve(
                    &archive,
                    isoplane_prepare::package::PackageLimits::of(&manifest)
                ),
                Err(PackageRefusal::Package(
                    isoplane_prepare::package::PackageError::TooManyFiles { limit: 4 }
                ))
            ),
            "a node must refuse a package of costless files at its file limit"
        );
    }

    #[test]
    fn an_application_package_is_held_to_its_configured_size() {
        let root = installation(&["1.0.0"]);
        let runtimes = Runtimes::load(root.path()).expect("loads");
        let free: isoplane_abi::config::AppManifest =
            toml::from_str("name = \"app\"\nlimits.module_bytes_compressed = 5242880\nlimits.package_bytes = 52428800\nlimits.instance_memory_max = 134217728\nlimits.occupancy_ms_per_http_dispatch = 100\nlimits.subrequests_per_dispatch = 20\nlimits.pending_futures_per_dispatch = 6").expect("an explicit small-budget manifest");
        let limits = isoplane_prepare::package::PackageLimits::of(&free);
        let maximum_package_bytes = 50 << 20;
        let identity = identity_json("1.0.0", MODULE);
        let package = |bytes: u64| {
            let mut builder = tar::Builder::new(Vec::new());
            let assembly = bytes - identity.len() as u64 - 2;
            for (path, contents) in [
                ("runtime.json", identity.as_bytes().to_vec()),
                ("application.json", b"{}".to_vec()),
                ("assemblies/App.dll", vec![0; assembly as usize]),
            ] {
                let mut header = tar::Header::new_ustar();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_entry_type(tar::EntryType::Regular);
                builder
                    .append_data(&mut header, path, contents.as_slice())
                    .expect("entry");
            }
            builder.into_inner().expect("archive")
        };
        assert!(
            runtimes
                .resolve(&package(maximum_package_bytes), limits)
                .is_ok(),
            "a 50 MiB package must be admitted on the configured application budget"
        );
        assert!(
            matches!(
                runtimes.resolve(&package(maximum_package_bytes + 1), limits),
                Err(PackageRefusal::Package(
                    isoplane_prepare::package::PackageError::TooLarge { .. }
                ))
            ),
            "a package one byte over 50 MiB must be refused on the configured application budget"
        );
    }

    #[test]
    fn an_empty_inventory_does_not_load() {
        let root = installation(&[]);
        assert!(
            Runtimes::load(root.path()).is_err(),
            "a node configured for packages must support at least one SDK release"
        );
    }
}
