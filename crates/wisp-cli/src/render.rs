//! Terminal output.

use std::fmt::Write as _;

pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn new(color: bool) -> Self {
        Style { color }
    }
    pub fn dim(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn bold(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn green(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[32m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn yellow(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[33m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
    pub fn red(&self, s: &str) -> String {
        if self.color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.into()
        }
    }
}

pub fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let mut s = String::with_capacity(width);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('·');
    }
    s
}

pub fn bytes_human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// `wisp doctor` — the capability manifest.
pub fn doctor(st: &Style, path: &str, r: &crate::doctor::Report) -> String {
    use crate::doctor::{kind_label, kind_note};
    let mut o = String::new();

    let _ = writeln!(o, "\n  {} {}", st.bold("wisp doctor"), path);
    let _ = writeln!(
        o,
        "  {} {}  blake3 {}",
        st.dim("module"),
        bytes_human(r.bytes as u64),
        st.dim(&r.blake3[..16])
    );

    let _ = writeln!(o, "\n  {}", st.bold("CAPABILITY MANIFEST"));
    let _ = writeln!(
        o,
        "  {}",
        st.dim("the complete, auditable list. Anything not below is unreachable.")
    );
    if r.declared.is_empty() {
        let _ = writeln!(o, "    {}", st.dim("(no platform imports at all)"));
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
            let _ = writeln!(
                o,
                "    {:<10} {:<20} {:<18} {}",
                st.dim(label),
                imp.name,
                kind_label(imp.kind),
                st.dim(kind_note(imp))
            );
            if !flags.is_empty() {
                let note = if imp.sensitive {
                    "results may contain sensitive data; guest state may retain copies"
                } else {
                    "allowed during initialization"
                };
                let _ = writeln!(
                    o,
                    "    {:<10} {:<20} {}",
                    "",
                    "",
                    st.dim(&format!("↳ {} · {}", flags.join(", "), note))
                );
            }
        }
    }

    if !r.pal.is_empty() {
        let _ = writeln!(o, "\n  {}", st.bold("WASI PAL ADAPTER"));
        for line in [
            "`wasi:*` imports the BCL emits, satisfied at deploy by composition. A delegated",
            "one IS the capability it names; a stubbed one is nothing. Neither adds a channel.",
        ] {
            let _ = writeln!(o, "  {}", st.dim(line));
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
            let _ = writeln!(o, "    {} {:<66} {}", mark, p.full_name(), st.dim(&note));
        }
    }

    let _ = writeln!(o, "\n  {}", st.bold("SO: THIS CODE CAN"));
    for line in r.plain_english() {
        let _ = writeln!(o, "    • {line}");
    }
    let _ = writeln!(
        o,
        "  {}",
        st.dim("  and nothing else — no threads, no files, no ambient sockets, no syscalls.")
    );

    let _ = writeln!(o, "\n  {}", st.bold("EXPORTS"));
    let mut line = String::from("    ");
    for e in wisp_abi::imports::exports::REQUIRED {
        let ok = !r.missing_required_exports.contains(e);
        let _ = write!(
            line,
            "{} {}   ",
            if ok { st.green("✓") } else { st.red("✗") },
            e
        );
    }
    let _ = writeln!(o, "{line}");
    let mut line = String::from("    ");
    for e in wisp_abi::imports::exports::OPTIONAL {
        let ok = !r.missing_optional_exports.contains(e);
        let _ = write!(
            line,
            "{} {}   ",
            if ok { st.green("✓") } else { st.dim("·") },
            st.dim(&format!("{e} (optional)"))
        );
    }
    let _ = writeln!(o, "{line}");

    let _ = writeln!(o, "\n  {}", st.bold("DEPLOY VERDICT"));
    if r.would_be_rejected() {
        for u in &r.undeclared {
            let _ = writeln!(
                o,
                "    {} V1  import `{}.{}` is not in the platform's capability manifest",
                st.red("✗"),
                u.module,
                u.field
            );
        }
        for e in &r.missing_required_exports {
            let _ = writeln!(o, "    {} required export `{e}` is missing", st.red("✗"));
        }
        let _ = writeln!(
            o,
            "\n    {} — this module would not deploy. There is no dynamic-linking escape hatch.",
            st.red("REJECTED"),
        );
    } else {
        let _ = writeln!(
            o,
            "    {} every import is a declared capability; every required export is present",
            st.green("✓")
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_proportional_and_bounded() {
        assert_eq!(bar(0.0, 4), "····");
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██··");
        assert_eq!(bar(9.0, 4), "████", "fractions above 1 clamp");
        assert_eq!(bar(-1.0, 4), "····");
    }

    #[test]
    fn byte_sizes_read_like_sizes() {
        assert_eq!(bytes_human(512), "512 B");
        assert_eq!(bytes_human(2048), "2.0 KB");
        assert_eq!(bytes_human(5 << 20), "5.0 MB");
    }

    #[test]
    fn style_is_plain_when_colour_is_off() {
        let st = Style::new(false);
        assert_eq!(st.red("x"), "x");
        assert_eq!(st.bold("x"), "x");
        assert!(Style::new(true).red("x").contains('\x1b'));
    }
}
