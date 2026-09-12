//! Project inputs and content-based change detection for local development.

use anyhow::Context;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use wisp_abi::config::{AppManifest, InitSandbox};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub sources: blake3::Hash,
    pub module: blake3::Hash,
}

#[derive(Clone)]
pub struct Project {
    pub root: PathBuf,
    pub manifest: AppManifest,
    pub tenant: String,
    pub module: PathBuf,
    pub init_sandbox: InitSandbox,
    pub build: Option<String>,
    pub allow_endpoints: Vec<SocketAddr>,
    pub platform_files: wisp_engine::PlatformFiles,
}

#[derive(Deserialize)]
struct Inputs {
    tenant: String,
    module: PathBuf,
    #[serde(default)]
    dev: Development,
}

#[derive(Default, Deserialize)]
struct Development {
    build: Option<String>,
    #[serde(default)]
    init_sandbox: InitSandbox,
    #[serde(default)]
    allow_endpoints: Vec<SocketAddr>,
    #[serde(default)]
    trust: wisp_trust::TrustConfig,
}

impl Project {
    pub fn load(
        root: &Path,
        build: Option<&str>,
        sandbox: Option<InitSandbox>,
        no_build: bool,
    ) -> anyhow::Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("opening project {}", root.display()))?;
        let path = root.join("wisp.toml");
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {}; local development requires name, tenant and module",
                path.display()
            )
        })?;
        let manifest: AppManifest =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut inputs: Inputs = toml::from_str(&text)
            .with_context(|| format!("parsing local inputs in {}", path.display()))?;
        anyhow::ensure!(
            !manifest.name.trim().is_empty(),
            "wisp.toml must establish a nonempty app name"
        );
        anyhow::ensure!(
            !inputs.tenant.trim().is_empty(),
            "wisp.toml must establish a nonempty tenant"
        );
        anyhow::ensure!(
            !inputs.module.as_os_str().is_empty(),
            "wisp.toml must name the module to execute"
        );
        anyhow::ensure!(
            inputs
                .dev
                .allow_endpoints
                .iter()
                .all(|address| address.port() != 0
                    && !address.ip().is_unspecified()
                    && !address.ip().is_multicast()),
            "dev.allow_endpoints must name individual IP addresses and nonzero ports"
        );
        if let Some(path) = &mut inputs.dev.trust.bundle {
            *path = root.join(&*path);
        }
        let trust = wisp_trust::TrustBundle::load(&inputs.dev.trust)?;
        let platform_files = wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
            path: wisp_trust::GUEST_PATH.to_owned(),
            bytes: trust.pem().into(),
            modified_ns: trust.modified_ns(),
        }])?;
        let command = if no_build {
            None
        } else {
            build.map(str::to_owned).or(inputs.dev.build).or_else(|| {
                root.join("build.sh")
                    .is_file()
                    .then(|| "bash build.sh".to_owned())
            })
        };
        Ok(Self {
            module: root.join(inputs.module),
            root,
            manifest,
            tenant: inputs.tenant,
            init_sandbox: sandbox.unwrap_or(inputs.dev.init_sandbox),
            build: command,
            allow_endpoints: inputs.dev.allow_endpoints,
            platform_files,
        })
    }

    pub fn state_dir(&self) -> PathBuf {
        let mut hash = blake3::Hasher::new();
        hash.update(&(self.tenant.len() as u64).to_le_bytes());
        hash.update(self.tenant.as_bytes());
        hash.update(self.manifest.name.as_bytes());
        self.root
            .join(".wisp")
            .join("state")
            .join(hash.finalize().to_hex().as_str())
    }

    pub fn fingerprint(&self, module_only: bool) -> anyhow::Result<Fingerprint> {
        let mut hash = blake3::Hasher::new();
        if module_only {
            hash_file(&self.root.join("wisp.toml"), &mut hash)?;
        } else {
            scan(&self.root, &self.module, &mut hash)?;
        }
        let sources = hash.finalize();
        let mut module = blake3::Hasher::new();
        hash_file(&self.module, &mut module)?;
        Ok(Fingerprint {
            sources,
            module: module.finalize(),
        })
    }
}

fn hash_file(path: &Path, hash: &mut blake3::Hasher) -> anyhow::Result<()> {
    hash.update(path.as_os_str().as_encoded_bytes());
    match std::fs::read(path) {
        Ok(bytes) => {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            hash.update(b"missing");
        }
        Err(e) => return Err(e).with_context(|| format!("watching {}", path.display())),
    }
    Ok(())
}

fn scan(dir: &Path, module: &Path, hash: &mut blake3::Hasher) -> anyhow::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "target" | "node_modules" | "bin" | "obj" | "out"
            )
        {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            scan(&entry.path(), module, hash)?;
        } else if kind.is_file() && entry.path() != module {
            hash_file(&entry.path(), hash)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_project_refuses_wildcard_service_exceptions() {
        let directory = tempfile::tempdir().unwrap();
        for endpoint in ["0.0.0.0:5432", "127.0.0.1:0", "224.0.0.1:5432"] {
            std::fs::write(directory.path().join("wisp.toml"), format!("name='app'\ntenant='owner'\nmodule='app.wasm'\n[dev]\nallow_endpoints=['{endpoint}']\n")).unwrap();
            assert!(
                Project::load(directory.path(), None, None, true).is_err(),
                "development services must not grant wildcard endpoint {endpoint}"
            );
        }
    }

    #[test]
    fn an_unverified_local_trust_override_is_refused_before_preparation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("wisp.toml"),
            "name='app'\ntenant='owner'\nmodule='app.wasm'\n[dev.trust]\nbundle='local-ca.pem'\n",
        )
        .unwrap();
        assert!(
            Project::load(directory.path(), None, None, true).is_err(),
            "a trust override without its expected digest must not start the app"
        );
    }

    #[test]
    fn a_project_requires_explicit_identity_and_a_module() {
        let dir = tempfile::tempdir().unwrap();
        for manifest in [
            "name=''\ntenant='owner'\nmodule='app.wasm'",
            "name='app'\ntenant=''\nmodule='app.wasm'",
            "name='app'\ntenant='owner'\nmodule=''",
            "name='app'\nmodule='app.wasm'",
        ] {
            std::fs::write(dir.path().join("wisp.toml"), manifest).unwrap();
            assert!(
                Project::load(dir.path(), None, None, true).is_err(),
                "incomplete project identity must be refused: {manifest}"
            );
        }
    }

    #[test]
    fn local_state_is_scoped_to_both_tenant_and_app() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wisp.toml"),
            "name='app'\ntenant='owner'\nmodule='app.wasm'",
        )
        .unwrap();
        let project = Project::load(dir.path(), None, None, true).unwrap();
        let original = project.state_dir();
        let mut changed = project.clone();
        changed.tenant = "other".into();
        assert_ne!(
            original,
            changed.state_dir(),
            "changing tenant must not inherit another tenant's persisted state"
        );
        changed = project;
        changed.manifest.name = "other".into();
        assert_ne!(
            original,
            changed.state_dir(),
            "changing app must not inherit another app's persisted state"
        );
    }

    #[test]
    fn cli_build_options_override_project_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wisp.toml"), "name='app'\ntenant='owner'\nmodule='app.wasm'\n[dev]\nbuild='project-build'\ninit_sandbox='runtime-bringup'").unwrap();
        let project = Project::load(
            dir.path(),
            Some("custom-build"),
            Some(InitSandbox::Sealed),
            false,
        )
        .unwrap();
        assert_eq!(
            project.build.as_deref(),
            Some("custom-build"),
            "the CLI build command must take precedence"
        );
        assert_eq!(
            project.init_sandbox,
            InitSandbox::Sealed,
            "the CLI sandbox must take precedence"
        );
        let prebuilt = Project::load(dir.path(), None, None, true).unwrap();
        assert!(
            prebuilt.build.is_none(),
            "--no-build must disable the configured project build"
        );
    }
    #[test]
    fn edits_deletions_and_module_replacements_are_observed_without_timestamp_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wisp.toml"),
            "name='example'\ntenant='owner'\nmodule='out/app.wasm'",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        let project = Project::load(dir.path(), None, None, true).unwrap();
        let initial = project.fingerprint(false).unwrap();
        let source = dir.path().join("handler.cs");
        std::fs::write(&source, "first").unwrap();
        let added = project.fingerprint(false).unwrap();
        assert_ne!(initial, added, "a new source must trigger a build");
        std::fs::write(&source, "other").unwrap();
        assert_ne!(
            added,
            project.fingerprint(false).unwrap(),
            "a same-size edit must trigger a build"
        );
        std::fs::remove_file(source).unwrap();
        assert_eq!(
            initial,
            project.fingerprint(false).unwrap(),
            "deletion must restore the original inputs"
        );
        std::fs::write(&project.module, "module").unwrap();
        assert_ne!(
            initial,
            project.fingerprint(false).unwrap(),
            "the configured module must be watched even under out/"
        );
    }
    #[test]
    fn external_build_mode_ignores_source_edits_until_module_publication() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wisp.toml"),
            "name='app'\ntenant='owner'\nmodule='app.wasm'",
        )
        .unwrap();
        let project = Project::load(dir.path(), None, None, true).unwrap();
        let before = project.fingerprint(true).unwrap();
        std::fs::write(dir.path().join("Application.cs"), "edited source").unwrap();
        assert_eq!(
            before,
            project.fingerprint(true).unwrap(),
            "external build mode must not reload stale code on source edits"
        );
        std::fs::write(dir.path().join("app.wasm"), "published module").unwrap();
        assert_ne!(
            before,
            project.fingerprint(true).unwrap(),
            "publishing new module bytes must trigger a runtime reload"
        );
    }

    #[test]
    fn build_outputs_do_not_create_reload_loops() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("wisp.toml"),
            "name='example'\ntenant='owner'\nmodule='out/app.wasm'",
        )
        .unwrap();
        let project = Project::load(dir.path(), None, None, true).unwrap();
        let before = project.fingerprint(false).unwrap();
        for name in ["obj", "bin", "target", ".wisp", "out"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name).join("output"), "new").unwrap();
        }
        assert_eq!(
            before,
            project.fingerprint(false).unwrap(),
            "intermediates must not trigger another build"
        );
    }
}
