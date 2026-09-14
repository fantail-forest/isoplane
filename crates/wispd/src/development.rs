//! File-backed application settings, outside deployment artifacts and node diagnostics.

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wisp_host::bindings::{RuntimeConfiguration, RuntimeConfigurationError};
use wisp_host::egress::{DenyReason, DestinationPolicy};

/// File references and endpoint exceptions for one established application owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSettings {
    /// Owning tenant, matched exactly.
    pub tenant: String,
    /// Application name, matched exactly within its tenant.
    pub application: String,
    /// Flat string-valued JSON, using colon-delimited .NET configuration keys.
    pub user_secrets_file: Option<PathBuf>,
    /// Exact additional destinations, applied only inside this application's worker.
    #[serde(default)]
    pub allowed_destinations: Vec<SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsError {
    #[error("development settings need unique nonempty tenant/application identities")]
    Owner,
    #[error("development destinations need a concrete IP address and nonzero port")]
    Destination,
    #[error("development destination exceptions require supervised application workers")]
    InProcess,
    #[error("reading development runtime settings: {0}")]
    Read(#[from] std::io::Error),
    #[error(transparent)]
    Configuration(#[from] RuntimeConfigurationError),
}

pub(crate) fn validate(config: &crate::config::NodeConfig) -> Result<(), SettingsError> {
    let mut owners = std::collections::HashSet::new();
    for entry in &config.development_applications {
        if entry.tenant.is_empty()
            || entry.application.is_empty()
            || !owners.insert((&entry.tenant, &entry.application))
        {
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

pub(crate) fn reject_in_process_destinations(
    config: &crate::config::NodeConfig,
) -> Result<(), SettingsError> {
    if config
        .development_applications
        .iter()
        .any(|entry| !entry.allowed_destinations.is_empty())
    {
        return Err(SettingsError::InProcess);
    }
    Ok(())
}

pub(crate) struct SettingsOwner<'a> {
    pub tenant: &'a str,
    pub application: &'a str,
}

#[derive(Clone, Default)]
pub(crate) struct RuntimeSettings {
    pub configuration: RuntimeConfiguration,
    pub allowed_destinations: Vec<SocketAddr>,
}

pub(crate) trait RuntimeSettingsSource: Send + Sync {
    fn settings(&self, owner: SettingsOwner<'_>) -> Result<RuntimeSettings, SettingsError>;
}

pub(crate) struct DevelopmentFiles(Vec<ApplicationSettings>);

impl DevelopmentFiles {
    pub fn configured(config: &crate::config::NodeConfig) -> Result<Arc<Self>, SettingsError> {
        validate(config)?;
        Ok(Arc::new(Self(config.development_applications.clone())))
    }

    pub fn open(config: &crate::config::NodeConfig) -> Result<Arc<Self>, SettingsError> {
        let source = Self::configured(config)?;
        for entry in &source.0 {
            source.settings(SettingsOwner {
                tenant: &entry.tenant,
                application: &entry.application,
            })?;
        }
        Ok(source)
    }
}

impl RuntimeSettingsSource for DevelopmentFiles {
    fn settings(&self, owner: SettingsOwner<'_>) -> Result<RuntimeSettings, SettingsError> {
        let Some(entry) = self
            .0
            .iter()
            .find(|entry| entry.tenant == owner.tenant && entry.application == owner.application)
        else {
            return Ok(RuntimeSettings::default());
        };
        let configuration = match &entry.user_secrets_file {
            Some(path) => {
                let mut bytes = Vec::new();
                std::fs::File::open(path)?
                    .take(wisp_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                RuntimeConfiguration::from_json(&bytes)?
            }
            None => RuntimeConfiguration::default(),
        };
        Ok(RuntimeSettings {
            configuration,
            allowed_destinations: entry.allowed_destinations.clone(),
        })
    }
}

pub(crate) struct WorkerSettings {
    pub tenant: String,
    pub application: String,
    pub settings: RuntimeSettings,
}

impl RuntimeSettingsSource for WorkerSettings {
    fn settings(&self, owner: SettingsOwner<'_>) -> Result<RuntimeSettings, SettingsError> {
        if self.tenant != owner.tenant || self.application != owner.application {
            return Err(SettingsError::Owner);
        }
        Ok(self.settings.clone())
    }
}

pub(crate) struct WorkerDestinations {
    pub endpoints: Vec<SocketAddr>,
    pub baseline: Arc<dyn DestinationPolicy>,
}

impl DestinationPolicy for WorkerDestinations {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if self.endpoints.contains(&address) {
            Ok(())
        } else {
            self.baseline.authorize(address)
        }
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if self
            .endpoints
            .iter()
            .any(|endpoint| endpoint.ip() == address)
        {
            Ok(())
        } else {
            self.baseline.authorize_resolution(address)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(path: PathBuf) -> crate::NodeConfig {
        let mut config = crate::NodeConfig::dev_default();
        config.development_applications.push(ApplicationSettings {
            tenant: "tenant-a".into(),
            application: "app-a".into(),
            user_secrets_file: Some(path),
            allowed_destinations: vec!["127.0.0.1:5432".parse().expect("endpoint")],
        });
        config
    }

    fn bytes(settings: RuntimeSettings) -> Vec<u8> {
        let mut output = Vec::new();
        settings
            .configuration
            .write_json(&mut output)
            .expect("private output");
        output
    }

    #[test]
    fn files_refresh_without_exposing_values_to_configuration_or_siblings() {
        let directory = tempfile::tempdir().expect("owned directory");
        let path = directory.path().join("secrets.json");
        let first = br#"{"ConnectionStrings:Db":"test-private-first"}"#;
        std::fs::write(&path, first).expect("settings");
        let config = configuration(path.clone());
        let source = DevelopmentFiles::open(&config).expect("source");
        assert_eq!(
            bytes(
                source
                    .settings(SettingsOwner {
                        tenant: "tenant-a",
                        application: "app-a"
                    })
                    .expect("owner")
            ),
            first,
            "the exact owner receives its runtime values"
        );
        for owner in [
            SettingsOwner {
                tenant: "tenant-b",
                application: "app-a",
            },
            SettingsOwner {
                tenant: "tenant-a",
                application: "app-b",
            },
        ] {
            let settings = source.settings(owner).expect("sibling");
            assert!(
                settings.allowed_destinations.is_empty(),
                "siblings cannot inherit endpoint exceptions"
            );
            assert_eq!(
                bytes(settings),
                b"{}",
                "siblings cannot inherit runtime values"
            );
        }
        assert!(
            !format!("{config:?}").contains("test-private-first"),
            "debug output cannot contain file values"
        );
        assert!(
            !serde_json::to_string(&config)
                .expect("config")
                .contains("test-private-first"),
            "merged configuration cannot contain file values"
        );
        let second = br#"{"ConnectionStrings:Db":"test-private-second"}"#;
        std::fs::write(path, second).expect("updated settings");
        assert_eq!(
            bytes(
                source
                    .settings(SettingsOwner {
                        tenant: "tenant-a",
                        application: "app-a"
                    })
                    .expect("fresh settings")
            ),
            second,
            "a subsequent worker start must read the current file"
        );
    }

    #[test]
    fn restarting_an_application_does_not_reread_a_siblings_missing_file() {
        let directory = tempfile::tempdir().expect("owned directory");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, b"{}").expect("owner settings");
        let mut config = configuration(path);
        let sibling_path = directory.path().join("sibling.json");
        std::fs::write(&sibling_path, b"{}").expect("sibling settings");
        let mut sibling = config.development_applications[0].clone();
        sibling.application = "app-b".into();
        sibling.user_secrets_file = Some(sibling_path.clone());
        config.development_applications.push(sibling);
        DevelopmentFiles::open(&config).expect("startup validates every dependency");
        std::fs::remove_file(sibling_path).expect("sibling file removed after startup");
        let source = DevelopmentFiles::configured(&config).expect("worker settings source");
        assert_eq!(
            bytes(
                source
                    .settings(SettingsOwner {
                        tenant: "tenant-a",
                        application: "app-a"
                    })
                    .expect("healthy owner")
            ),
            b"{}",
            "a healthy application can restart when a sibling file is absent"
        );
        assert!(
            matches!(
                source.settings(SettingsOwner {
                    tenant: "tenant-a",
                    application: "app-b"
                }),
                Err(SettingsError::Read(_))
            ),
            "the affected application must still refuse missing settings"
        );
    }

    #[test]
    fn invalid_application_ownership_is_refused() {
        let mut config = configuration(PathBuf::from("unused"));
        config
            .development_applications
            .push(config.development_applications[0].clone());
        assert!(
            matches!(validate(&config), Err(SettingsError::Owner)),
            "duplicate owners are ambiguous"
        );
        config.development_applications.pop();
        config.development_applications[0].tenant.clear();
        assert!(
            matches!(validate(&config), Err(SettingsError::Owner)),
            "tenant identity cannot default"
        );
    }

    #[test]
    fn invalid_and_oversized_files_are_refused_without_echoing_values() {
        let directory = tempfile::tempdir().expect("owned directory");
        let path = directory.path().join("secrets.json");
        let config = configuration(path.clone());
        for invalid in [
            b"{\"test-private\":42}".to_vec(),
            vec![b' '; wisp_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES + 1],
        ] {
            std::fs::write(&path, invalid).expect("invalid settings");
            let error = match DevelopmentFiles::open(&config) {
                Ok(_) => panic!("invalid settings accepted"),
                Err(error) => error,
            };
            assert!(
                matches!(error, SettingsError::Configuration(_)),
                "invalid files must fail before worker start"
            );
            assert!(
                !error.to_string().contains("test-private"),
                "parse errors cannot echo values"
            );
        }
        std::fs::remove_file(path).expect("remove source");
        assert!(
            matches!(DevelopmentFiles::open(&config), Err(SettingsError::Read(_))),
            "missing configuration cannot silently default"
        );
    }

    #[test]
    fn exceptions_are_exact_and_require_an_isolated_worker() {
        let mut config = configuration(PathBuf::from("unused"));
        assert!(
            matches!(
                reject_in_process_destinations(&config),
                Err(SettingsError::InProcess)
            ),
            "in-process apps cannot share configured exceptions"
        );
        for invalid in ["0.0.0.0:5432", "224.0.0.1:5432", "127.0.0.1:0"] {
            config.development_applications[0].allowed_destinations =
                vec![invalid.parse().expect("endpoint")];
            assert!(
                matches!(validate(&config), Err(SettingsError::Destination)),
                "invalid destination must be refused"
            );
        }
        let destinations = WorkerDestinations {
            endpoints: vec!["127.0.0.1:5432".parse().expect("endpoint")],
            baseline: Arc::new(wisp_host::egress::PublicDestinations),
        };
        assert!(
            destinations
                .authorize("127.0.0.1:5432".parse().expect("endpoint"))
                .is_ok(),
            "configured endpoint must be reachable"
        );
        assert!(
            destinations
                .authorize_resolution("127.0.0.1".parse().expect("ip"))
                .is_ok(),
            "configured resolution must be reachable"
        );
        for denied in ["127.0.0.1:5433", "127.0.0.2:5432"] {
            assert!(
                destinations
                    .authorize(denied.parse().expect("endpoint"))
                    .is_err(),
                "unconfigured endpoints remain denied"
            );
        }
        assert!(
            destinations
                .authorize_resolution("127.0.0.2".parse().expect("ip"))
                .is_err(),
            "unconfigured private resolution remains denied"
        );
    }

    #[test]
    fn a_worker_refuses_settings_for_another_owner() {
        let source = WorkerSettings {
            tenant: "tenant-a".into(),
            application: "app-a".into(),
            settings: RuntimeSettings::default(),
        };
        assert_eq!(
            bytes(
                source
                    .settings(SettingsOwner {
                        tenant: "tenant-a",
                        application: "app-a"
                    })
                    .expect("owner")
            ),
            b"{}",
            "worker serves its established owner"
        );
        assert!(
            matches!(
                source.settings(SettingsOwner {
                    tenant: "tenant-b",
                    application: "app-a"
                }),
                Err(SettingsError::Owner)
            ),
            "a worker cannot install a sibling tenant"
        );
    }
}
