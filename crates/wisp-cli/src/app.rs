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

/// Resolve application budgets and an optional memory-only override.
pub fn resolve_limits(
    memory_flag: Option<u64>,
    module: &std::path::Path,
) -> anyhow::Result<(wisp_abi::config::Limits, String)> {
    let from_module = module
        .canonicalize()
        .ok()
        .and_then(|module| module.parent().map(std::path::Path::to_path_buf));
    let mut selected = None;
    for start in [from_module, std::env::current_dir().ok()]
        .into_iter()
        .flatten()
    {
        let mut directory = Some(start.as_path());
        while let Some(path) = directory {
            if let Some((manifest, source)) = CliManifest::load(path)? {
                selected = Some(match manifest.limits {
                    Some(limits) => (limits, format!("limits in {}", source.display())),
                    None => (
                        wisp_abi::config::Limits::default(),
                        format!(
                            "{} declares no `limits`; using the default",
                            source.display()
                        ),
                    ),
                });
                break;
            }
            directory = path.parent();
        }
        if selected.is_some() {
            break;
        }
    }
    let (mut limits, source) = selected.unwrap_or_else(|| {
        (
            wisp_abi::config::Limits::default(),
            "no wisp.toml found; using the default".into(),
        )
    });
    if let Some(memory) = memory_flag {
        limits.instance_memory_max = memory;
        return Ok((
            limits,
            format!("--guest-memory-bytes; other limits from {source}"),
        ));
    }
    Ok((limits, source))
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
