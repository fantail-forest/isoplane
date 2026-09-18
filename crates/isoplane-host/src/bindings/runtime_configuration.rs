//! Host-resident activation configuration, isolated by registered module owner.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES;
use parking_lot::RwLock;

use crate::ModuleId;

/// Validated UTF-8 JSON configuration. Values may be captured in prepared artifacts.
#[derive(Clone)]
pub struct RuntimeConfiguration(Arc<[u8]>);

/// Invalid configuration or an attempt to configure another module owner.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeConfigurationError {
    /// The supplied blob exceeds the host allocation limit.
    #[error(
        "runtime configuration is {actual_bytes} bytes; limit is {MAX_RUNTIME_CONFIGURATION_BYTES}"
    )]
    TooLarge { actual_bytes: usize },
    /// Configuration must be a JSON object whose values are strings.
    #[error("runtime configuration must be a JSON object of string values")]
    InvalidJson,
    /// Configuration can only be attached to a registered module.
    #[error("runtime configuration module is not registered")]
    UnknownModule,
    /// The caller's tenant does not own the registered module.
    #[error("runtime configuration tenant does not own the registered module")]
    WrongTenant,
}

impl RuntimeConfiguration {
    /// Validates a bounded JSON object of string keys and string values.
    pub fn from_json(bytes: &[u8]) -> Result<Self, RuntimeConfigurationError> {
        if bytes.len() > MAX_RUNTIME_CONFIGURATION_BYTES {
            return Err(RuntimeConfigurationError::TooLarge {
                actual_bytes: bytes.len(),
            });
        }
        serde_json::from_slice::<BTreeMap<String, String>>(bytes)
            .map_err(|_| RuntimeConfigurationError::InvalidJson)?;
        Ok(Self(Arc::from(bytes)))
    }

    /// Writes the validated JSON to an explicitly selected private transport.
    pub fn write_json(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&self.0)
    }

    pub(super) fn fill(&self, out: &mut [u8]) -> i64 {
        let copied = out.len().min(self.0.len());
        out[..copied].copy_from_slice(&self.0[..copied]);
        self.0.len() as i64
    }
}

impl Default for RuntimeConfiguration {
    fn default() -> Self {
        Self(Arc::from(&b"{}"[..]))
    }
}

/// Per-cell configuration storage; guest identity comes from its instance context.
#[derive(Default)]
pub struct RuntimeConfigurations(RwLock<HashMap<(String, ModuleId), RuntimeConfiguration>>);

impl RuntimeConfigurations {
    pub(crate) fn set(&self, tenant: &str, module: &ModuleId, configuration: RuntimeConfiguration) {
        self.0
            .write()
            .insert((tenant.to_owned(), module.clone()), configuration);
    }

    pub(super) fn get(&self, tenant: &str, module: &ModuleId) -> RuntimeConfiguration {
        self.0
            .read()
            .get(&(tenant.to_owned(), module.clone()))
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_requires_a_bounded_string_map() {
        for invalid in [
            b"[]".as_slice(),
            b"null",
            b"{\"secret\":42}",
            b"{\"secret\":\"sensitive",
        ] {
            assert!(
                matches!(
                    RuntimeConfiguration::from_json(invalid),
                    Err(RuntimeConfigurationError::InvalidJson)
                ),
                "non-string maps must be refused without echoing values"
            );
        }
        let oversized = vec![b' '; MAX_RUNTIME_CONFIGURATION_BYTES + 1];
        assert!(
            matches!(
                RuntimeConfiguration::from_json(&oversized),
                Err(RuntimeConfigurationError::TooLarge { .. })
            ),
            "configuration allocation must be bounded before parsing"
        );
        let mut maximum = b"{}".to_vec();
        maximum.resize(MAX_RUNTIME_CONFIGURATION_BYTES, b' ');
        assert!(
            RuntimeConfiguration::from_json(&maximum).is_ok(),
            "the documented maximum must be accepted"
        );
    }

    #[test]
    fn configuration_returns_full_length_for_probes_and_partial_copies() {
        let bytes = br#"{"ConnectionStrings:Db":"secret"}"#;
        let config = RuntimeConfiguration::from_json(bytes).expect("valid configuration");
        assert_eq!(
            config.fill(&mut []),
            bytes.len() as i64,
            "a probe must return the required capacity"
        );
        let mut out = [0; 3];
        assert_eq!(
            config.fill(&mut out),
            bytes.len() as i64,
            "a short buffer must still report the full length"
        );
        assert_eq!(&out, &bytes[..3], "only the available prefix may be copied");
    }

    #[test]
    fn configuration_is_isolated_by_tenant_application_and_version() {
        let configurations = RuntimeConfigurations::default();
        let module = ModuleId::new("app", "v1");
        configurations.set(
            "tenant",
            &module,
            RuntimeConfiguration::from_json(br#"{"key":"secret"}"#).expect("valid map"),
        );
        for (tenant, candidate) in [
            ("other", module.clone()),
            ("tenant", ModuleId::new("other", "v1")),
            ("tenant", ModuleId::new("app", "v2")),
        ] {
            let mut out = [0; 2];
            assert_eq!(
                configurations.get(tenant, &candidate).fill(&mut out),
                2,
                "another owner or version must receive only the empty default"
            );
            assert_eq!(
                &out, b"{}",
                "configuration must not cross module ownership boundaries"
            );
        }
        assert!(
            configurations.get("tenant", &module).fill(&mut []) > 2,
            "the matching owner must receive its configured map"
        );
    }
}
