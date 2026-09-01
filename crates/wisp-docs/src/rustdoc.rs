//! Reading rustdoc's JSON output.
//!
//! `cargo rustdoc -- -Z unstable-options --output-format json` is the only
//! machine-readable view of a Rust crate's public surface that rustc itself
//! produces, which makes it the right input for a generated API page: it is
//! the same analysis `cargo doc` renders, so the page cannot describe a
//! function the compiler does not have.
//!
//! ## Why `serde_json` and not `rustdoc-types`
//!
//! `rustdoc-types` gives typed access, at the cost of pinning one
//! `format_version`: when nightly bumps the schema, the crate must bump too,
//! and until it does the failure is a deserialize error somewhere deep in a
//! generated type. Reading the handful of fields this generator needs out of
//! `serde_json::Value`, with [`FORMAT_VERSION`] asserted up front, gives the
//! same detection with a message that says what to do — and tolerates the
//! additive changes that make up most schema churn.
//!
//! The trade is real in the other direction too: nothing type-checks the field
//! names below. [`Crate::load`] is what buys that back, by refusing a document
//! whose version it has not been read against.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value;

/// The rustdoc JSON schema this reader was written against.
///
/// A mismatch is a hard failure, not a warning: the field names below are
/// unchecked by the compiler, so a schema this code has not been read against
/// could silently produce an empty or wrong page — which is worse than no
/// page.
pub const FORMAT_VERSION: u64 = 61;

/// A parsed rustdoc JSON document.
pub struct Crate {
    pub root: String,
    index: BTreeMap<String, Value>,
    /// Set by [`render_type`] when it meets a construct it cannot spell.
    pub unrendered: std::cell::RefCell<Vec<String>>,
}

/// Output directory for this generator's guest-workspace rustdoc invocations.
/// Keeping it separate avoids sharing Cargo's build-directory lock with an outer build.
pub fn target_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target/wisp-docs/rustdoc")
}

/// Builds the JSON for one package in the guest workspace and parses it.
pub fn build(workspace_root: &Path, package: &str) -> Result<Crate> {
    let target = target_dir(workspace_root);
    let out = Command::new("cargo")
        .current_dir(workspace_root)
        .env("CARGO_TARGET_DIR", &target)
        // Cargo's own state, inherited into a nested invocation, makes cargo
        // behave as if it were still the outer build.
        .env_remove("CARGO_PRIMARY_PACKAGE")
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .args([
            "+nightly",
            "rustdoc",
            "--quiet",
            "-p",
            package,
            // The SDK is `cfg(target_arch = "wasm32")` throughout — `testing`
            // is entirely `cfg(not(wasm32))` — so documenting for the host
            // would describe a surface no guest ever links.
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
            "guests/Cargo.toml",
            "--",
            "-Z",
            "unstable-options",
            "--output-format",
            "json",
        ])
        .output()
        .context("running cargo +nightly rustdoc")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "cargo +nightly rustdoc failed for `{package}`.\n\
             This page is generated from rustdoc's JSON output, which needs the nightly \
             toolchain and the wasm32 target:\n\
             \x20   rustup toolchain install nightly\n\
             \x20   rustup +nightly target add wasm32-unknown-unknown\n\n{err}"
        );
    }
    let path = target.join("wasm32-unknown-unknown/doc").join(format!("{}.json", package.replace('-', "_")));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    Crate::load(&text)
}

impl Crate {
    pub fn load(text: &str) -> Result<Crate> {
        let v: Value = serde_json::from_str(text).context("parsing rustdoc JSON")?;
        let got = v["format_version"].as_u64().unwrap_or(0);
        anyhow::ensure!(
            got == FORMAT_VERSION,
            "rustdoc JSON is format_version {got}, this reader was written against \
             {FORMAT_VERSION}. Re-read crates/wisp-docs/src/rustdoc.rs against the new schema \
             and update FORMAT_VERSION; do not just bump the constant."
        );
        let index = v["index"]
            .as_object()
            .context("rustdoc JSON has no `index`")?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Crate {
            root: v["root"].to_string().trim_matches('"').to_string(),
            index,
            unrendered: std::cell::RefCell::new(Vec::new()),
        })
    }

    pub fn get(&self, id: &str) -> Option<&Value> {
        self.index.get(id)
    }

    /// The `inner` kind of an item: `"struct"`, `"function"`, …
    pub fn kind(&self, id: &str) -> Option<String> {
        let it = self.get(id)?;
        it["inner"].as_object()?.keys().next().cloned()
    }

    pub fn name(&self, id: &str) -> String {
        self.get(id).and_then(|i| i["name"].as_str()).unwrap_or("").to_string()
    }

    pub fn docs(&self, id: &str) -> String {
        self.get(id).and_then(|i| i["docs"].as_str()).unwrap_or("").to_string()
    }

    pub fn is_public(&self, id: &str) -> bool {
        self.get(id).map(|i| i["visibility"] == "public").unwrap_or(false)
    }

    /// Child ids of a module, in declaration order.
    pub fn module_items(&self, id: &str) -> Vec<String> {
        self.get(id)
            .and_then(|i| i["inner"]["module"]["items"].as_array())
            .map(|a| a.iter().map(|v| v.to_string().trim_matches('"').to_string()).collect())
            .unwrap_or_default()
    }

    /// A `fn` signature, spelled the way it appears in source.
    pub fn signature(&self, id: &str) -> String {
        let Some(it) = self.get(id) else { return String::new() };
        let sig = &it["inner"]["function"]["sig"];
        let inputs: Vec<String> = sig["inputs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        let n = p[0].as_str().unwrap_or("_");
                        let t = self.render_type(&p[1]);
                        // `&self` prints as `&self`, not `self: &Self`.
                        if n == "self" {
                            match t.as_str() {
                                "Self" => "self".into(),
                                "&Self" => "&self".into(),
                                "&mut Self" => "&mut self".into(),
                                other => format!("self: {other}"),
                            }
                        } else {
                            format!("{n}: {t}")
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let out = match sig.get("output") {
            Some(Value::Null) | None => String::new(),
            Some(t) => format!(" -> {}", self.render_type(t)),
        };
        let is_async = it["inner"]["function"]["header"]["is_async"].as_bool().unwrap_or(false);
        let is_const = it["inner"]["function"]["header"]["is_const"].as_bool().unwrap_or(false);
        let prefix = format!(
            "{}{}",
            if is_const { "const " } else { "" },
            if is_async { "async " } else { "" }
        );
        format!("{prefix}fn {}({}){out}", self.name(id), inputs.join(", "))
    }

    /// A type, as Rust source.
    ///
    /// Anything this does not know how to spell is recorded in
    /// [`Crate::unrendered`] rather than silently rendered as a guess — a
    /// reference page that quietly prints the wrong type is worse than one
    /// that fails to build.
    pub fn render_type(&self, t: &Value) -> String {
        match t {
            Value::String(s) if s == "infer" => "_".into(),
            Value::Object(o) => {
                let Some((k, v)) = o.iter().next() else { return "_".into() };
                match k.as_str() {
                    "resolved_path" => {
                        let path = v["path"].as_str().unwrap_or("_");
                        format!("{path}{}", self.render_args(&v["args"]))
                    }
                    "generic" => v.as_str().unwrap_or("_").to_string(),
                    "primitive" => v.as_str().unwrap_or("_").to_string(),
                    "tuple" => {
                        let parts: Vec<String> = v
                            .as_array()
                            .map(|a| a.iter().map(|x| self.render_type(x)).collect())
                            .unwrap_or_default();
                        if parts.is_empty() {
                            "()".into()
                        } else {
                            format!("({})", parts.join(", "))
                        }
                    }
                    "slice" => format!("[{}]", self.render_type(v)),
                    "array" => format!(
                        "[{}; {}]",
                        self.render_type(&v["type"]),
                        v["len"].as_str().unwrap_or("_")
                    ),
                    "borrowed_ref" => format!(
                        "&{}{}",
                        if v["is_mutable"].as_bool().unwrap_or(false) { "mut " } else { "" },
                        self.render_type(&v["type"])
                    ),
                    "raw_pointer" => format!(
                        "*{} {}",
                        if v["is_mutable"].as_bool().unwrap_or(false) { "mut" } else { "const" },
                        self.render_type(&v["type"])
                    ),
                    "impl_trait" => format!("impl {}", self.render_bounds(v)),
                    "dyn_trait" => format!("dyn {}", self.render_bounds(&v["traits"])),
                    "qualified_path" => {
                        let name = v["name"].as_str().unwrap_or("_");
                        format!("{}::{name}", self.render_type(&v["self_type"]))
                    }
                    "function_pointer" => "fn(..)".into(),
                    other => {
                        self.unrendered.borrow_mut().push(other.to_string());
                        format!("«{other}»")
                    }
                }
            }
            other => {
                self.unrendered.borrow_mut().push(other.to_string());
                "«?»".into()
            }
        }
    }

    fn render_bounds(&self, v: &Value) -> String {
        let Some(a) = v.as_array() else { return "_".into() };
        let parts: Vec<String> = a
            .iter()
            .filter_map(|b| {
                if let Some(tb) = b.get("trait_bound") {
                    let p = tb["trait"]["path"].as_str().unwrap_or("_");
                    Some(format!("{p}{}", self.render_args(&tb["trait"]["args"])))
                } else if let Some(o) = b.get("outlives") {
                    Some(o.as_str().unwrap_or("'_").to_string())
                } else {
                    // A `dyn Trait` entry is the bare trait object shape.
                    b.get("trait").map(|t| {
                        let p = t["path"].as_str().unwrap_or("_");
                        format!("{p}{}", self.render_args(&t["args"]))
                    })
                }
            })
            .collect();
        parts.join(" + ")
    }

    fn render_args(&self, v: &Value) -> String {
        let Some(ab) = v.get("angle_bracketed") else { return String::new() };
        let mut parts: Vec<String> = ab["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        if let Some(t) = x.get("type") {
                            Some(self.render_type(t))
                        } else {
                            x.get("lifetime").and_then(|l| l.as_str()).map(str::to_string)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(cs) = ab["constraints"].as_array() {
            for c in cs {
                let name = c["name"].as_str().unwrap_or("_");
                if let Some(eq) = c["binding"].get("equality") {
                    parts.push(format!("{name} = {}", self.render_type(&eq["type"])));
                }
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("<{}>", parts.join(", "))
        }
    }
}

/// The first paragraph of a doc comment, folded to one line.
pub fn summary(docs: &str) -> String {
    let first = docs.split("\n\n").next().unwrap_or("");
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> Crate {
        Crate::load(r#"{"format_version":61,"root":"0","index":{}}"#).unwrap()
    }

    #[test]
    fn a_schema_this_reader_has_not_seen_is_refused() {
        let err = match Crate::load(r#"{"format_version":999,"root":"0","index":{}}"#) {
            Ok(_) => panic!("a format_version this reader has not seen must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("do not just bump the constant"), "{err}");
    }

    #[test]
    fn types_render_as_rust_source() {
        let c = c();
        let t = |s: &str| c.render_type(&serde_json::from_str::<Value>(s).unwrap());
        assert_eq!(t(r#"{"primitive":"u32"}"#), "u32");
        assert_eq!(t(r#"{"tuple":[]}"#), "()");
        assert_eq!(
            t(r#"{"borrowed_ref":{"is_mutable":true,"type":{"primitive":"u8"}}}"#),
            "&mut u8"
        );
        assert_eq!(t(r#"{"slice":{"primitive":"u8"}}"#), "[u8]");
        assert_eq!(
            t(r#"{"resolved_path":{"path":"Vec","args":{"angle_bracketed":{"args":[{"type":{"primitive":"u8"}}],"constraints":[]}}}}"#),
            "Vec<u8>"
        );
        assert!(c.unrendered.borrow().is_empty());
    }

    /// Proven to fire: an unknown construct is recorded, not guessed at.
    #[test]
    fn an_unknown_type_construct_is_recorded_rather_than_invented() {
        let c = c();
        let out = c.render_type(&serde_json::from_str::<Value>(r#"{"brand_new_kind":{}}"#).unwrap());
        assert_eq!(out, "«brand_new_kind»");
        assert_eq!(c.unrendered.borrow().len(), 1);
    }
}
