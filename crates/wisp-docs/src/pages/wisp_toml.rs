//! `wisp.toml` — from [`wisp_abi::config::AppManifest`] and
//! [`wisp_cli::app::CliManifest`].
//!
//! The page makes a point of the two layers: the contract the runtime consumes
//! and the handful of keys only the CLI needs. Both are read here, so the page
//! cannot claim a key that neither struct has.

use anyhow::Result;
use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle};

use crate::table::{cell, Annotations, Table};
use crate::Block;

const PAGE: &str = "wisp-toml.md";

/// An application manifest with only its required name, to read optional defaults.
fn manifest_defaults() -> Result<AppManifest> {
    Ok(toml::from_str("name = \"app\"\n")?)
}


const CLI_NOTES: &[(&str, &str)] = &[
    ("name", "Application name"),
    ("tenant", "Owning tenant"),
    ("module", "Path to the built WebAssembly module"),
    ("hosts", "Application hostnames"),
    ("path_prefix", "Routing path prefix"),
    ("limits", "Numeric resource budgets"),
];

const BINDING_NOTES: &[(&str, &str)] =
    &[("fetch_allowlist", "Optional outbound HTTP host allowlist")];





/// Generates application-manifest reference tables.
pub fn blocks() -> Result<Vec<Block>> {
    Ok(vec![
        Block::new(PAGE, "wisp-cli-keys", cli_keys()?),
        Block::new(PAGE, "wisp-handlers", handlers()?),
        Block::new(PAGE, "wisp-bindings", bindings()?),
    ])
}

fn q(s: &str) -> String {
    format!("`{s}`")
}


/// The CLI manifest keys and their deserialization defaults.
fn cli_keys() -> Result<String> {
    let wisp_cli::app::CliManifest {
        name,
        tenant,
        module,
        hosts,
        path_prefix,
        handlers: _,
        limits,
    } = wisp_cli::app::CliManifest::default();
    let optional = |value: Option<String>| {
        value
            .map(|value| q(&format!("{value:?}")))
            .unwrap_or_else(|| "none".into())
    };
    let mut notes = Annotations::new("wisp.toml CLI key", CLI_NOTES);
    let mut t = Table::new(&["Key", "Type", "Default", "Meaning"]);
    for (key, ty, default) in [
        ("name", "string", optional(name)),
        ("tenant", "string", optional(tenant)),
        ("module", "path", optional(module)),
        ("hosts", "string[]", q(&format!("{hosts:?}"))),
        ("path_prefix", "string", optional(path_prefix)),
        (
            "limits",
            "table",
            limits
                .map(|value| q(&format!("{value:?}")))
                .unwrap_or_else(|| "none".into()),
        ),
    ] {
        t.row(&[q(key), ty.into(), default, notes.get(key)]);
    }
    notes.finish()?;
    Ok(t.render())
}

fn handlers() -> Result<String> {
    let HandlerConfig { lifecycle } = manifest_defaults()?.handlers;
    let mut t = Table::new(&["Key", "Type", "Default", "Meaning"]);
    let values = [HandlerLifecycle::Reuse, HandlerLifecycle::Fresh]
        .iter()
        .map(|l| format!("`{}`", format!("{l:?}").to_lowercase()))
        .collect::<Vec<_>>()
        .join(" \\| ");
    t.row(&[
        q("lifecycle"),
        values,
        q(&format!("{lifecycle:?}").to_lowercase()),
        cell("Handler freshness. `fresh` resets the instance after every dispatch."),
    ]);
    Ok(t.render())
}

fn bindings() -> Result<String> {
    let Bindings { fetch_allowlist: _ } = manifest_defaults()?.bindings;
    let mut notes = Annotations::new("wisp.toml binding", BINDING_NOTES);
    let mut t = Table::new(&["Key", "Type", "Meaning"]);
    t.row(&[
        q("fetch_allowlist"),
        "optional string[]".into(),
        notes.get("fetch_allowlist"),
    ]);
    notes.finish()?;
    Ok(t.render())
}




#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::config::Limits;


    #[test]
    fn omitted_optional_manifest_fields_use_their_serde_defaults() {
        let m = manifest_defaults().unwrap();
        assert_eq!(m.handlers.lifecycle, HandlerLifecycle::Reuse);
        assert_eq!(m.limits, Limits::default());
    }
}
