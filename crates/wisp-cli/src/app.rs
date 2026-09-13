//! Application identity, numeric budgets, lifecycle and local artifact location.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CliManifest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub handlers: Handlers,
    #[serde(default)]
    /// Optional numeric resource budgets from the local application manifest.
    pub limits: Option<wisp_abi::config::Limits>,
    /// Optional local memory ceiling, independent of the selected limits.
    #[serde(default)]
    pub guest_memory_bytes: Option<wisp_abi::config::GuestMemoryBytes>,
    /// Structural admission budgets from the local manifest.
    #[serde(default)]
    pub validation_limits: wisp_abi::config::ValidationLimits,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Handlers {
    #[serde(default)]
    pub lifecycle: Option<String>,
}

impl CliManifest {
    pub fn load(dir: &std::path::Path) -> anyhow::Result<Option<(Self, std::path::PathBuf)>> {
        let path = dir.join("wisp.toml");
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some((toml::from_str(&text)?, path)))
    }

    /// The nearest `wisp.toml` at or above `start`, for a `doctor` pointed at
    /// a built artifact away from its app.
    pub fn find(start: &std::path::Path) -> anyhow::Result<Option<(Self, std::path::PathBuf)>> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if let Some(found) = Self::load(d)? {
                return Ok(Some(found));
            }
            dir = d.parent();
        }
        Ok(None)
    }

    pub fn lifecycle(&self) -> Option<wisp_abi::config::HandlerLifecycle> {
        match self.handlers.lifecycle.as_deref() {
            Some("fresh") => Some(wisp_abi::config::HandlerLifecycle::Fresh),
            Some("reuse") => Some(wisp_abi::config::HandlerLifecycle::Reuse),
            _ => None,
        }
    }
}

/// Effective doctor policy and the configuration source of each selection.
pub struct DoctorPolicy {
    /// Numeric budgets supplying non-memory limits and the default memory ceiling.
    pub limits: wisp_abi::config::Limits,
    /// Effective structural admission budgets.
    pub validation_limits: wisp_abi::config::ValidationLimits,
    /// Flag, manifest location, or default that selected the limits.
    pub limits_from: String,
    /// Independent memory ceiling, if configured.
    pub guest_memory_bytes: Option<wisp_abi::config::GuestMemoryBytes>,
    /// Flag or manifest location supplying memory, or `limits` for its default.
    pub guest_memory_from: String,
}

/// Resolve one application manifest, then apply an independent memory override.
pub fn resolve_doctor_policy(
    memory_flag: Option<wisp_abi::config::GuestMemoryBytes>,
    module: &std::path::Path,
) -> anyhow::Result<DoctorPolicy> {
    let manifest = doctor_manifest(module)?;
    let (limits, limits_from) = match &manifest {
        Some((manifest, source)) => match manifest.limits {
            Some(limits) => (limits, format!("limits in {}", source.display())),
            None => (
                wisp_abi::config::Limits::default(),
                format!(
                    "{} declares no `limits`; using the default",
                    source.display()
                ),
            ),
        },
        None => (
            wisp_abi::config::Limits::default(),
            "no wisp.toml found; using the default".into(),
        ),
    };
    let (guest_memory_bytes, guest_memory_from) = if let Some(memory) = memory_flag {
        (Some(memory), "--guest-memory-bytes".into())
    } else if let Some((manifest, source)) = &manifest {
        match manifest.guest_memory_bytes {
            Some(memory) => (
                Some(memory),
                format!("guest_memory_bytes in {}", source.display()),
            ),
            None => (None, "limits".into()),
        }
    } else {
        (None, "limits".into())
    };
    Ok(DoctorPolicy {
        validation_limits: manifest
            .as_ref()
            .map(|(m, _)| m.validation_limits)
            .unwrap_or_default(),
        limits,
        limits_from,
        guest_memory_bytes,
        guest_memory_from,
    })
}

fn doctor_manifest(
    module: &std::path::Path,
) -> anyhow::Result<Option<(CliManifest, std::path::PathBuf)>> {
    let from_module = module
        .canonicalize()
        .ok()
        .and_then(|module| module.parent().map(|directory| directory.to_path_buf()));
    for start in [from_module, std::env::current_dir().ok()]
        .into_iter()
        .flatten()
    {
        if let Some(found) = CliManifest::find(&start)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// `v-<epoch-secs>-<hash8>`: sortable, and anchored to the module bytes.
pub fn derive_version(module: Option<&[u8]>, now_s: u64) -> String {
    let h = match module {
        Some(bytes) => hex::encode(&blake3::hash(bytes).as_bytes()[..4]),
        None => "00000000".into(),
    };
    format!("v-{now_s}-{h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_anchored_to_the_module_contents() {
        let a = derive_version(Some(b"module a"), 100);
        let b = derive_version(Some(b"module b"), 100);
        assert_ne!(
            a, b,
            "different bytes must be a different immutable version"
        );
        assert_eq!(
            a,
            derive_version(Some(b"module a"), 100),
            "the same bytes at the same second are the same version"
        );
        assert!(
            a.starts_with("v-100-"),
            "the second comes first, so ids sort"
        );
    }

    #[test]
    fn a_manifest_maps_onto_the_abi_types() {
        let m: CliManifest = toml::from_str(
            r#"
name = "quotes"
tenant = "acme"
module = "target/quotes.wasm"
hosts = ["quotes.wisp.dev"]
limits = { module_bytes_compressed = 5242880, instance_memory_max = 67108864, cpu_ms_per_http_dispatch = 10, subrequests_per_dispatch = 20, pending_futures_per_dispatch = 6 }

[handlers]
lifecycle = "fresh"
"#,
        )
        .unwrap();
        assert_eq!(m.name.as_deref(), Some("quotes"), "name is read");
        assert_eq!(
            m.limits,
            Some(wisp_abi::config::Limits {
                module_bytes_compressed: 5242880,
                instance_memory_max: 67108864,
                cpu_ms_per_http_dispatch: 10,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6
            }),
            "`limits` maps onto the limits"
        );
        assert_eq!(
            m.lifecycle(),
            Some(wisp_abi::config::HandlerLifecycle::Fresh),
            "`handlers.lifecycle` maps onto the lifecycle"
        );
        assert_eq!(m.hosts, vec!["quotes.wisp.dev"], "hosts are read");
    }

    #[test]
    fn an_empty_manifest_is_legal() {
        let m: CliManifest = toml::from_str("").unwrap();
        assert!(
            m.name.is_none() && m.limits.is_none(),
            "every field is optional"
        );
    }
}
