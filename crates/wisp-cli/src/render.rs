//! Terminal output.

/// Output assembled a line at a time.
#[derive(Default)]
struct Out(String);

impl Out {
    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        self.0.push_str(&args.to_string());
        self.0.push('\n');
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_proportional_and_bounded() {
        assert_eq!(bar(0.0, 4), "····", "empty");
        assert_eq!(bar(1.0, 4), "████", "full");
        assert_eq!(bar(0.5, 4), "██··", "half");
        assert_eq!(bar(9.0, 4), "████", "fractions above 1 clamp");
        assert_eq!(bar(-1.0, 4), "····", "fractions below 0 clamp");
    }

    #[test]
    fn byte_sizes_read_like_sizes() {
        assert_eq!(bytes_human(512), "512 B", "bytes stay bytes");
        assert_eq!(bytes_human(2048), "2.0 KB", "kilobytes");
        assert_eq!(bytes_human(5 << 20), "5.0 MB", "megabytes");
    }

    #[test]
    fn style_is_plain_when_colour_is_off() {
        let st = Style::new(false);
        assert_eq!(st.red("x"), "x", "no escape codes without colour");
        assert_eq!(st.bold("x"), "x", "no escape codes without colour");
        assert!(
            Style::new(true).red("x").contains('\x1b'),
            "escape codes with colour"
        );
    }



}
