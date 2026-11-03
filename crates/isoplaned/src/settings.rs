//! App-specific local settings, read for preparation and captured with the deployed artifact.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use isoplane_host::bindings::{RuntimeConfiguration, RuntimeConfigurationError};
use serde::{Deserialize, Serialize};

/// Local configuration source and endpoint exceptions for one established application owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSettings {
    /// Application name, matched exactly within the host owner.
    pub application: String,
    /// Owner-only flat string-valued JSON, using colon-delimited .NET configuration keys.
    /// Values may include secrets and are never included in node diagnostics.
    pub configuration_file: Option<PathBuf>,
    /// Exact additional destinations reachable by this application's instances.
    #[serde(default)]
    pub allowed_destinations: Vec<SocketAddr>,
}

/// A source cannot provide a valid private snapshot for the requested owner.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Owner attribution is missing or duplicated in the source configuration.
    #[error("application settings need unique nonempty application identities")]
    Owner,
    /// Endpoint exceptions must name individual IP/port pairs.
    #[error("application destinations need a concrete IP address and nonzero port")]
    Destination,
    /// A selected file is missing, inaccessible or insufficiently protected.
    #[error(transparent)]
    File(#[from] isoplane_control::private_files::PrivateFileError),
    /// A file is corrupt or exceeds the bounded configuration format.
    #[error("invalid application settings for `{tenant}/{application}`: {source}")]
    Configuration {
        /// Established owner; configuration values are omitted.
        tenant: String,
        /// Established application name.
        application: String,
        /// A bounded error which never echoes input JSON.
        source: RuntimeConfigurationError,
    },
}

pub(crate) fn validate(entries: &[ApplicationSettings]) -> Result<(), SettingsError> {
    let mut owners = std::collections::HashSet::new();
    for entry in entries {
        if entry.application.trim().is_empty() || !owners.insert(&entry.application) {
            return Err(SettingsError::Owner);
        }
        for address in &entry.allowed_destinations {
            if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
                return Err(SettingsError::Destination);
            }
        }
    }
    Ok(())
}

/// Established ownership passed to a configuration source; never inferred from guest input.
#[derive(Clone, Copy)]
pub struct SettingsOwner<'a> {
    /// Established tenant identity.
    pub tenant: &'a str,
    /// Application within that tenant.
    pub application: &'a str,
}

/// One immutable selection used for both initialization and activation of a deployment.
#[derive(Clone, Default)]
pub struct RuntimeSettings {
    /// Guest-visible configuration; treat its bytes as secret-bearing.
    pub configuration: RuntimeConfiguration,
    /// Exact additional destinations bound to this selection.
    pub allowed_destinations: Vec<SocketAddr>,
}

/// Supplies a fresh snapshot for a new preparation. Existing deployments retain their snapshot.
pub trait RuntimeSettingsSource: Send + Sync {
    /// Reads only this owner's selected source. Missing configured data is an error.
    fn settings(&self, owner: SettingsOwner<'_>) -> Result<RuntimeSettings, SettingsError>;
}

/// Protected local files, with explicit application ownership and no live-reload side effects.
pub struct LocalFiles {
    owner: String,
    entries: Vec<ApplicationSettings>,
}

impl LocalFiles {
    /// Validates source definitions without opening other applications' dependencies.
    pub fn configured(
        owner: String,
        entries: Vec<ApplicationSettings>,
    ) -> Result<Arc<Self>, SettingsError> {
        if owner.trim().is_empty() {
            return Err(SettingsError::Owner);
        }
        validate(&entries)?;
        Ok(Arc::new(Self { owner, entries }))
    }
}

impl RuntimeSettingsSource for LocalFiles {
    fn settings(&self, owner: SettingsOwner<'_>) -> Result<RuntimeSettings, SettingsError> {
        if owner.tenant != self.owner || owner.application.trim().is_empty() {
            return Err(SettingsError::Owner);
        }
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| self.owner == owner.tenant && entry.application == owner.application)
        else {
            return Ok(RuntimeSettings::default());
        };
        let configuration = match &entry.configuration_file {
            Some(path) => {
                let bytes = isoplane_control::private_files::read(
                    path,
                    isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES + 1,
                )?;
                RuntimeConfiguration::from_json(&bytes).map_err(|source| {
                    SettingsError::Configuration {
                        tenant: owner.tenant.to_owned(),
                        application: owner.application.to_owned(),
                        source,
                    }
                })?
            }
            None => RuntimeConfiguration::default(),
        };
        Ok(RuntimeSettings {
            configuration,
            allowed_destinations: entry.allowed_destinations.clone(),
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use isoplane_control::private_files::PrivateFileError;
    use std::io::Write;

    fn entry(path: PathBuf) -> ApplicationSettings {
        ApplicationSettings {
            application: "app".into(),
            configuration_file: Some(path),
            allowed_destinations: vec!["127.0.0.1:5432".parse().unwrap()],
        }
    }
    fn owner() -> SettingsOwner<'static> {
        SettingsOwner {
            tenant: "owner",
            application: "app",
        }
    }
    fn bytes(settings: &RuntimeSettings) -> Vec<u8> {
        let mut bytes = Vec::new();
        settings.configuration.write_json(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn new_selections_reread_only_the_owner_while_existing_snapshots_remain_stable() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(br#"{"key":"private-first"}"#).unwrap();
        let source =
            LocalFiles::configured("owner".into(), vec![entry(file.path().to_owned())]).unwrap();
        let first = source.settings(owner()).unwrap();
        std::fs::write(file.path(), br#"{"key":"private-second"}"#).unwrap();
        assert_eq!(bytes(&first), br#"{"key":"private-first"}"#);
        assert_eq!(
            bytes(&source.settings(owner()).unwrap()),
            br#"{"key":"private-second"}"#
        );
        let sibling = source
            .settings(SettingsOwner {
                tenant: "owner",
                application: "another",
            })
            .unwrap();
        assert_eq!(bytes(&sibling), b"{}");
        assert!(sibling.allowed_destinations.is_empty());
        assert!(matches!(
            source.settings(SettingsOwner {
                tenant: "another",
                application: "app"
            }),
            Err(SettingsError::Owner)
        ));
        assert!(matches!(
            source.settings(SettingsOwner {
                tenant: "",
                application: "app"
            }),
            Err(SettingsError::Owner)
        ));
    }

    #[test]
    fn missing_corrupt_and_unprotected_sources_are_distinct_and_do_not_echo_values() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(br#"{"private-value":42}"#).unwrap();
        let source =
            LocalFiles::configured("owner".into(), vec![entry(file.path().to_owned())]).unwrap();
        let error = source.settings(owner()).err().unwrap();
        assert!(matches!(error, SettingsError::Configuration { .. }));
        assert!(!error.to_string().contains("private-value"));
        std::fs::write(
            file.path(),
            vec![b' '; isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES + 1],
        )
        .unwrap();
        assert!(matches!(
            source.settings(owner()),
            Err(SettingsError::Configuration {
                source: RuntimeConfigurationError::TooLarge { .. },
                ..
            })
        ));
        {
            let _permissions =
                isoplane_test_support::TemporaryPermissions::mode(file.path(), 0o644).unwrap();
            assert!(matches!(
                source.settings(owner()),
                Err(SettingsError::File(PrivateFileError::Protection(_)))
            ));
        }
        let path = file.path().to_owned();
        drop(file);
        assert!(
            matches!(source.settings(owner()), Err(SettingsError::File(PrivateFileError::Missing(missing))) if missing == path)
        );
    }

    #[test]
    fn preparation_does_not_open_sibling_sources() {
        let first = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(first.path(), b"{}").unwrap();
        let second = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(second.path(), b"{}").unwrap();
        let mut sibling = entry(second.path().to_owned());
        sibling.application = "sibling".into();
        let entries = vec![entry(first.path().to_owned()), sibling];
        LocalFiles::configured("owner".into(), entries.clone()).unwrap();
        drop(second);

        let source = LocalFiles::configured("owner".into(), entries).unwrap();
        assert_eq!(bytes(&source.settings(owner()).unwrap()), b"{}");
    }

    #[test]
    fn duplicate_or_blank_owners_and_inexact_destinations_are_rejected() {
        let mut first = entry("unused".into());
        assert!(matches!(
            validate(&[first.clone(), first.clone()]),
            Err(SettingsError::Owner)
        ));
        for address in ["0.0.0.0:1", "224.0.0.1:1", "127.0.0.1:0"] {
            first.allowed_destinations = vec![address.parse().unwrap()];
            assert!(matches!(
                validate(&[first.clone()]),
                Err(SettingsError::Destination)
            ));
        }
        first.allowed_destinations.clear();
        first.application = "  ".into();
        assert!(matches!(validate(&[first]), Err(SettingsError::Owner)));
    }
}
