//! Rendering `wisp doctor`'s capability manifest, and its JSON form.

use super::{bytes_human, Out, Style};

/// The capability manifest.
pub fn doctor(st: &Style, path: &str, r: &crate::doctor::Report) -> String {
    use crate::doctor::{kind_label, kind_note};
    let mut o = Out::default();

    o.line(format_args!("\n  {} {}", st.bold("wisp doctor"), path));
    o.line(format_args!(
        "  {} {}  blake3 {}",
        st.dim("module"),
        bytes_human(r.bytes as u64),
        st.dim(&r.blake3[..16])
    ));

    o.line(format_args!("\n  {}", st.bold("CAPABILITY MANIFEST")));
    o.line(format_args!(
        "  {}",
        st.dim("the complete, auditable list. Anything not below is unreachable.")
    ));
    if r.declared.is_empty() {
        o.line(format_args!(
            "    {}",
            st.dim("(no platform imports at all)")
        ));
    }
    for (group, imports) in r.by_group() {
        for (i, imp) in imports.iter().enumerate() {
            let label = if i == 0 { group } else { "" };
            let mut flags = Vec::new();
            if imp.sensitive {
                flags.push("sensitive");
            }
            if imp.init_visible {
                flags.push("init-visible");
            }
            o.line(format_args!(
                "    {:<10} {:<20} {:<18} {}",
                st.dim(label),
                imp.name,
                kind_label(imp.kind),
                st.dim(kind_note(imp))
            ));
            if !flags.is_empty() {
                let note = if imp.sensitive {
                    "results may contain sensitive data; guest state may retain copies"
                } else {
                    "allowed during initialization"
                };
                o.line(format_args!(
                    "    {:<10} {:<20} {}",
                    "",
                    "",
                    st.dim(&format!("↳ {} · {}", flags.join(", "), note))
                ));
            }
        }
    }

    if !r.pal.is_empty() {
        o.line(format_args!("\n  {}", st.bold("WASI PAL ADAPTER")));
        for line in [
            "`wasi:*` imports the BCL emits, satisfied at deploy by composition. A delegated",
            "one IS the capability it names; a stubbed one is nothing. Neither adds a channel.",
        ] {
            o.line(format_args!("  {}", st.dim(line)));
        }
        for p in &r.pal {
            let (mark, note) = match p.disposition.capability() {
                Some(cap) => (st.green("→"), format!("delegates to wisp.{cap}")),
                None if p.disposition.is_delegated() => (
                    st.dim("∅"),
                    "returns the empty value — the platform has no such state".into(),
                ),
                None => (st.yellow("!"), "stub: traps when called".into()),
            };
            o.line(format_args!(
                "    {} {:<66} {}",
                mark,
                p.full_name(),
                st.dim(&note)
            ));
        }
    }

    o.line(format_args!("\n  {}", st.bold("SO: THIS CODE CAN")));
    for line in r.plain_english() {
        o.line(format_args!("    • {line}"));
    }
    o.line(format_args!(
        "  {}",
        st.dim("  and nothing else — no threads, no files, no ambient sockets, no syscalls.")
    ));


    o.line(format_args!("\n  {}", st.bold("EXPORTS")));
    let mut line = String::from("    ");
    for e in wisp_abi::imports::exports::REQUIRED {
        let ok = !r.missing_required_exports.contains(e);
        line.push_str(&format!(
            "{} {}   ",
            if ok { st.green("✓") } else { st.red("✗") },
            e
        ));
    }
    o.line(format_args!("{line}"));
    let mut line = String::from("    ");
    for e in wisp_abi::imports::exports::OPTIONAL {
        let ok = !r.missing_optional_exports.contains(e);
        line.push_str(&format!(
            "{} {}   ",
            if ok { st.green("✓") } else { st.dim("·") },
            st.dim(&format!("{e} (optional)"))
        ));
    }
    o.line(format_args!("{line}"));

    o.line(format_args!("\n  {}", st.bold("DEPLOY VERDICT")));
    if r.would_be_rejected() {
        for u in &r.undeclared {
            o.line(format_args!(
                "    {} V1  import `{}.{}` is not in the platform's capability manifest",
                st.red("✗"),
                u.module,
                u.field
            ));
        }
        for e in &r.missing_required_exports {
            o.line(format_args!(
                "    {} required export `{e}` is missing",
                st.red("✗")
            ));
        }
        o.line(format_args!(
            "\n    {} — this module would not deploy. There is no dynamic-linking escape hatch.",
            st.red("REJECTED"),
        ));
    } else {
        o.line(format_args!(
            "    {} every import is a declared capability; every required export is present",
            st.green("✓")
        ));
    }
    o.0
}

/// A violation as JSON, behind a trait so `--json` has the same shape with
/// and without the validator linked in.
pub trait AsViolationJson {
    fn as_json(&self) -> serde_json::Value;
}

impl AsViolationJson for () {
    fn as_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

#[cfg(feature = "control")]
impl AsViolationJson for wisp_runtime::Violation {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": format!("{:?}", self.code),
            "subject": self.subject,
            "detail": self.detail,
        })
    }
}

/// Render the validation verdict and its effective limits and memory ceiling as JSON.
pub fn doctor_json<V: AsViolationJson>(
    r: &crate::doctor::Report,
    limits: wisp_abi::config::Limits,
    limits_from: &str,
    guest_memory_bytes: Option<wisp_abi::config::GuestMemoryBytes>,
    rejected: bool,
    violations: &[V],
) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "bytes": r.bytes,
        "blake3": r.blake3,
        "limits": limits,
        "limits_from": limits_from,
        "guest_memory_bytes": guest_memory_bytes.map_or(limits.instance_memory_max, |bytes| bytes.get()),
        "guest_memory_from": if guest_memory_bytes.is_some() { "--guest-memory-bytes" } else { "limits" },
        "validator": cfg!(feature = "control"),
        "violations": violations.iter().map(AsViolationJson::as_json).collect::<Vec<_>>(),
        "rejected": rejected,
        "capabilities": r.declared.iter().map(|i| serde_json::json!({
            "name": i.name,
            "group": crate::doctor::group_name(i.group),
            "kind": crate::doctor::kind_label(i.kind),
            "sensitive": i.sensitive,
            "init_visible": i.init_visible,
        })).collect::<Vec<_>>(),
        "undeclared": r.undeclared.iter().map(|u| format!("{}.{}", u.module, u.field)).collect::<Vec<_>>(),
        "pal": r.pal.iter().map(|p| serde_json::json!({
            "name": p.full_name(),
            "delegates_to": p.disposition.capability(),
            "traps": !p.disposition.is_delegated(),
        })).collect::<Vec<_>>(),
        "missing_required_exports": r.missing_required_exports,
        "missing_optional_exports": r.missing_optional_exports,
    }))?)
}
