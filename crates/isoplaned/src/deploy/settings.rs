//! Private settings captured with a prepared artifact, never refreshed during restoration.

use serde::{Deserialize, Serialize};

use super::{Deployer, PreparedReceipt};
use crate::settings::RuntimeSettings;

const FILE: &str = "runtime-settings.json";
const MAX_BYTES: usize = 8 * isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    tenant: String,
    application: String,
    version: String,
    manifest_sha256: String,
    configuration: Vec<u8>,
    allowed_destinations: Vec<std::net::SocketAddr>,
}

impl Deployer {
    pub(super) async fn write_runtime_settings(
        &self,
        directory: &std::path::Path,
        tenant: &str,
        receipt: &PreparedReceipt,
        configuration: &isoplane_host::bindings::RuntimeConfiguration,
        allowed_destinations: Vec<std::net::SocketAddr>,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !tenant.trim().is_empty(),
            "prepared settings require an established owner"
        );
        let mut bytes = Vec::new();
        configuration.write_json(&mut bytes)?;
        let snapshot = Snapshot {
            tenant: tenant.to_owned(),
            application: receipt.manifest.name.clone(),
            version: receipt.version.clone(),
            manifest_sha256: receipt.manifest_sha256.clone(),
            configuration: bytes,
            allowed_destinations,
        };
        let plain = serde_json::to_vec(&snapshot)?;
        anyhow::ensure!(
            plain.len() <= MAX_BYTES,
            "prepared settings exceed the protected snapshot bound"
        );
        let digest = blake3::hash(&plain);
        isoplane_control::private_files::write_new(&directory.join(FILE), &plain)?;
        Ok(digest.to_hex().to_string())
    }

    /// Reads the private settings used to initialize this exact prepared artifact.
    /// Never consults current source files or falls back when a snapshot is damaged or missing.
    pub async fn runtime_settings(
        &self,
        tenant: &str,
        receipt: &PreparedReceipt,
    ) -> anyhow::Result<RuntimeSettings> {
        anyhow::ensure!(
            !tenant.trim().is_empty(),
            "prepared settings require an established owner"
        );
        let digest: [u8; 32] = hex::decode(&receipt.settings_digest)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("prepared settings digest must contain 32 bytes"))?;
        let path = self.root.join(&receipt.artifact_key.0).join(FILE);
        let plain = isoplane_control::private_files::read(&path, MAX_BYTES + 1)?;
        anyhow::ensure!(
            plain.len() <= MAX_BYTES,
            "prepared settings exceed the private snapshot bound"
        );
        anyhow::ensure!(
            blake3::hash(&plain).as_bytes() == &digest,
            "prepared settings digest mismatch"
        );
        let snapshot: Snapshot = serde_json::from_slice(&plain)
            .map_err(|_| anyhow::anyhow!("prepared settings snapshot is corrupt"))?;
        anyhow::ensure!(
            snapshot.tenant == tenant
                && snapshot.application == receipt.manifest.name
                && snapshot.version == receipt.version
                && snapshot.manifest_sha256 == receipt.manifest_sha256,
            "prepared settings do not belong to this owner and artifact"
        );
        Ok(RuntimeSettings {
            configuration: isoplane_host::bindings::RuntimeConfiguration::from_json(
                &snapshot.configuration,
            )?,
            allowed_destinations: snapshot.allowed_destinations,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::settings::{ApplicationSettings, LocalFiles, RuntimeSettingsSource, SettingsOwner};

    fn bytes(settings: &RuntimeSettings) -> Vec<u8> {
        let mut bytes = Vec::new();
        settings.configuration.write_json(&mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn restored_artifacts_keep_their_private_settings_until_a_new_preparation_selects_the_source(
    ) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let registry = root.path().join("registry");
        let deployer = Deployer::open(&registry).unwrap();
        let source_path = root.path().join("application.json");
        isoplane_control::private_files::write_new(&source_path, br#"{"key":"private-first"}"#)
            .unwrap();
        let source = LocalFiles::configured(
            "owner".into(),
            vec![ApplicationSettings {
                application: "app".into(),
                configuration_file: Some(source_path.clone()),
                allowed_destinations: vec!["127.0.0.1:5432".parse().unwrap()],
            }],
        )
        .unwrap();
        let owner = SettingsOwner {
            tenant: "owner",
            application: "app",
        };
        let initial = source.settings(owner).unwrap();
        let artifact = registry.join("artifact-first");
        isoplane_control::private_files::directory(&artifact).unwrap();
        let mut receipt = PreparedReceipt {
            version: "v1".into(),
            artifact_key: "artifact-first".to_string().try_into().unwrap(),
            manifest_sha256: "a".repeat(64),
            settings_digest: String::new(),
            runtime_profile: isoplane_prepare::RuntimeProfile::raw_wasm(),
            manifest: serde_json::from_value(serde_json::json!({"name":"app"})).unwrap(),
        };
        receipt.settings_digest = deployer
            .write_runtime_settings(
                &artifact,
                "owner",
                &receipt,
                &initial.configuration,
                initial.allowed_destinations.clone(),
            )
            .await
            .unwrap();
        let captured = isoplane_control::private_files::read(&artifact.join(FILE), MAX_BYTES + 1)
            .expect("captured settings remain private");
        let captured_snapshot: Snapshot = serde_json::from_slice(&captured).unwrap();
        assert_eq!(
            captured_snapshot.configuration,
            br#"{"key":"private-first"}"#
        );
        std::fs::write(&source_path, br#"{"key":"private-second"}"#).unwrap();
        assert_eq!(
            bytes(&source.settings(owner).unwrap()),
            br#"{"key":"private-second"}"#
        );
        assert_eq!(
            bytes(&deployer.runtime_settings("owner", &receipt).await.unwrap()),
            br#"{"key":"private-first"}"#
        );
        drop(deployer);
        std::fs::remove_file(source_path).unwrap();
        let restarted = Deployer::open(&registry).unwrap();
        let restored = restarted.runtime_settings("owner", &receipt).await.unwrap();
        assert_eq!(bytes(&restored), br#"{"key":"private-first"}"#);
        assert_eq!(restored.allowed_destinations, initial.allowed_destinations);
        assert!(restarted
            .runtime_settings("another-owner", &receipt)
            .await
            .is_err());
        for mutate in [0, 1, 2] {
            let mut other = receipt.clone();
            match mutate {
                0 => other.manifest.name = "other-app".into(),
                1 => other.version = "v2".into(),
                _ => other.manifest_sha256 = "b".repeat(64),
            }
            assert!(restarted.runtime_settings("owner", &other).await.is_err());
        }
        let mut damaged = captured;
        damaged[12] ^= 1;
        std::fs::write(artifact.join(FILE), damaged).unwrap();
        assert!(restarted.runtime_settings("owner", &receipt).await.is_err());
        std::fs::remove_file(artifact.join(FILE)).unwrap();
        let missing = restarted
            .runtime_settings("owner", &receipt)
            .await
            .err()
            .unwrap();
        assert!(matches!(
            missing.downcast_ref::<isoplane_control::private_files::PrivateFileError>(),
            Some(isoplane_control::private_files::PrivateFileError::Missing(
                _
            ))
        ));
    }
}
