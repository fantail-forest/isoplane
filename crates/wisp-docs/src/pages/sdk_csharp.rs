//! Generate the C# reference from compiler XML documentation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::table::{cell, Table};

const PAGE: &str = "sdk-csharp.md";
const PROJECT: &str = "guest-sdks/dotnet/Wisp.Sdk.csproj";

/// Namespaces, in the order the page presents them, with the heading each gets.
///
/// A partition, checked below: a type in no listed namespace fails the build
/// rather than dropping off the page.
const NAMESPACES: &[(&str, &str)] = &[
    ("Wisp", "The surface a guest writes against"),
    ("Wisp.Durable", "Handler routing"),
    ("Wisp.Abi", "The ABI layer"),
    ("Wisp.Runtime", "The async runtime"),
];

/// One documented member.
struct Member {
    /// `T`, `M`, `P`, `F` or `E` — the compiler's own kind prefix.
    kind: char,
    /// Fully-qualified, without the kind prefix.
    full: String,
    summary: String,
}

pub fn pages() -> Result<Vec<(&'static str, String)>> {
    Ok(vec![(PAGE, render()?)])
}

/// Builds the SDK and returns the doc XML.
///
/// The path is pinned rather than inferred from the configuration, so the
/// generator reads the file this invocation just wrote and never a stale one
/// from some earlier build.
fn doc_xml() -> Result<String> {
    let root = crate::workspace_root();
    let out_dir = root.join("target/wisp-docs/dotnet");
    std::fs::create_dir_all(&out_dir)?;
    let xml: PathBuf = out_dir.join("Wisp.Sdk.xml");
    let out = Command::new("dotnet")
        .current_dir(&root)
        .args([
            "build",
            PROJECT,
            "-v",
            "q",
            "--nologo",
            &format!("-p:DocumentationFile={}", xml.display()),
        ])
        .output()
        .context(
            "running `dotnet build` for the C# SDK. This page is generated from the compiler's \
             XML doc output, so it needs the .NET SDK on PATH.",
        )?;
    if !out.status.success() {
        anyhow::bail!(
            "dotnet build failed for {PROJECT}.\n\
             `CS1591` is an error in this project on purpose: a public member with no doc \
             comment would render as a blank row on sdk-csharp.md. Document it.\n\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    std::fs::read_to_string(&xml).with_context(|| format!("reading {}", xml.display()))
}

fn render() -> Result<String> {
    let xml = doc_xml()?;
    let doc = roxmltree::Document::parse(&xml).context("parsing the C# doc XML")?;

    let mut members = Vec::new();
    for m in doc.descendants().filter(|n| n.has_tag_name("member")) {
        let Some(name) = m.attribute("name") else { continue };
        let Some((k, full)) = name.split_once(':') else { continue };
        let Some(kind) = k.chars().next() else { continue };
        let summary = m
            .children()
            .find(|c| c.has_tag_name("summary"))
            .map(|s| markdown(s))
            .unwrap_or_default();
        members.push(Member { kind, full: full.to_string(), summary });
    }
    anyhow::ensure!(!members.is_empty(), "sdk-csharp.md: the doc XML held no members");

    // Types, and the members that belong to each.
    let types: Vec<&Member> = members.iter().filter(|m| m.kind == 'T').collect();
    let mut owned: BTreeMap<&str, Vec<&Member>> = BTreeMap::new();
    for m in members.iter().filter(|m| m.kind != 'T') {
        // `M:Wisp.Response.Text(System.String,System.Int32)` belongs to
        // `T:Wisp.Response`.
        let head = m.full.split('(').next().unwrap_or(&m.full);
        let Some((ty, _)) = head.rsplit_once('.') else { continue };
        if let Some(t) = types.iter().find(|t| t.full == ty) {
            owned.entry(t.full.as_str()).or_default().push(m);
        }
    }

    let mut s = String::new();
    s.push_str("---\nid: sdk-csharp\ntitle: C# SDK\nsidebar_position: 11\n---\n\n# C# SDK\n\n");
    s.push_str(
        "**The target ecosystem.** `guest-sdks/dotnet` is one library every guest references; \
         it names no tenant type, so a guest supplies a router and the entrypoint is generic \
         over it.\n\nThis page is generated from that library's own XML doc comments by \
         `cargo run -p wisp-docs`. To change a description here, change the doc comment there \
         — and note that `CS1591` is an error in `Wisp.Sdk.csproj`, so a public member cannot \
         be added without one.\n",
    );

    // The partition check, before anything is rendered.
    for t in &types {
        let ns = namespace_of(&t.full);
        anyhow::ensure!(
            NAMESPACES.iter().any(|(n, _)| *n == ns),
            "sdk-csharp.md: type `{}` is in namespace `{ns}`, which the page does not present. \
             Add it to NAMESPACES in crates/wisp-docs — a public type on no page is an \
             undocumented surface.",
            t.full
        );
    }

    for (ns, heading) in NAMESPACES {
        let mut here: Vec<&&Member> =
            types.iter().filter(|t| namespace_of(&t.full) == *ns).collect();
        here.sort_by(|a, b| a.full.cmp(&b.full));
        if here.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {heading} {{#{}}}\n", anchor(ns)));
        s.push_str(&format!("\nNamespace `{ns}`.\n"));
        for t in here {
            s.push_str(&type_section(t, owned.get(t.full.as_str())));
        }
    }
    Ok(s)
}

fn namespace_of(full: &str) -> &str {
    full.rsplit_once('.').map(|(ns, _)| ns).unwrap_or("")
}

fn anchor(ns: &str) -> String {
    ns.replace('.', "-").to_lowercase()
}

fn type_section(t: &Member, members: Option<&Vec<&Member>>) -> String {
    let short = short_name(&t.full);
    let mut s = format!("\n### `{short}`\n\n");
    if !t.summary.trim().is_empty() {
        s.push_str(t.summary.trim());
        s.push('\n');
    }
    let Some(ms) = members else { return s };
    let mut rows: Vec<&&Member> = ms.iter().collect();
    rows.sort_by_key(|m| signature(&m.full));
    if rows.is_empty() {
        return s;
    }
    let mut table = Table::new(&["Member", "Meaning"]);
    for m in rows {
        table.row(&[format!("`{}`", signature(&m.full)), cell(&one_line(&m.summary))]);
    }
    s.push('\n');
    s.push_str(&table.render());
    s
}

/// `Wisp.Response` -> `Response`, and `Wisp.Abi.Exports\`1` -> `Exports<T>`.
fn short_name(full: &str) -> String {
    let last = full.rsplit('.').next().unwrap_or(full);
    generic_arity(last)
}

/// The compiler spells a generic type `Foo\`2`. Render the arity as the
/// placeholders a reader would type.
fn generic_arity(name: &str) -> String {
    match name.split_once('`') {
        Some((base, n)) => {
            let n: usize = n.parse().unwrap_or(0);
            let args: Vec<String> = (0..n).map(|i| format!("T{}", i + 1)).collect();
            format!("{base}<{}>", args.join(", "))
        }
        None => name.to_string(),
    }
}

/// A member's signature, as a C# reader would write it.
fn signature(full: &str) -> String {
    let (head, args) = match full.split_once('(') {
        Some((h, a)) => (h, Some(a.trim_end_matches(')'))),
        None => (full, None),
    };
    let name = generic_arity(head.rsplit('.').next().unwrap_or(head));
    // The compiler's constructor name is `#ctor`.
    let name = if name == "#ctor" { "new".to_string() } else { name };
    match args {
        None => name,
        Some("") => format!("{name}()"),
        Some(a) => {
            let parts: Vec<String> = split_args(a).into_iter().map(|t| csharp_type(&t)).collect();
            format!("{name}({})", parts.join(", "))
        }
    }
}

/// Splits an argument list without breaking generic arguments at their commas.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '{' | '<' | '(' => {
                depth += 1;
                cur.push(c);
            }
            '}' | '>' | ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `System.String` -> `string`, so the page reads like C# rather than like IL.
fn csharp_type(t: &str) -> String {
    let t = t.trim();
    let (base, suffix) = match t.strip_suffix("[]") {
        Some(b) => (b, "[]"),
        None => (t, ""),
    };
    let keyword = match base {
        "System.String" => "string",
        "System.Int32" => "int",
        "System.UInt32" => "uint",
        "System.Int64" => "long",
        "System.UInt64" => "ulong",
        "System.Byte" => "byte",
        "System.Boolean" => "bool",
        "System.Double" => "double",
        "System.Object" => "object",
        "System.Void" => "void",
        other => other.rsplit('.').next().unwrap_or(other),
    };
    format!("{keyword}{suffix}")
}

/// Renders one XML doc element's mixed content as markdown.
fn markdown(n: roxmltree::Node) -> String {
    let mut out = String::new();
    for c in n.children() {
        if c.is_text() {
            out.push_str(c.text().unwrap_or(""));
        } else if c.is_element() {
            match c.tag_name().name() {
                // `<c>` is inline code; `<code>` is a block.
                "c" => out.push_str(&format!("`{}`", markdown(c).trim())),
                "code" => out.push_str(&format!("\n\n```csharp\n{}\n```\n\n", markdown(c).trim())),
                // A cref is a link in a real doc site; here the target is on
                // the same page under a different name, so the readable thing
                // is the symbol itself.
                "see" | "seealso" => {
                    let r = c
                        .attribute("cref")
                        .map(|v| v.split_once(':').map(|(_, r)| r).unwrap_or(v).to_string())
                        .unwrap_or_default();
                    out.push_str(&format!("`{}`", signature(&r)));
                }
                "paramref" | "typeparamref" => {
                    out.push_str(&format!("`{}`", c.attribute("name").unwrap_or("")))
                }
                "para" => out.push_str(&format!("\n\n{}\n\n", markdown(c).trim())),
                "em" | "i" => out.push_str(&format!("*{}*", markdown(c).trim())),
                "b" | "strong" => out.push_str(&format!("**{}**", markdown(c).trim())),
                "list" | "item" | "description" | "term" => out.push_str(&markdown(c)),
                _ => out.push_str(&markdown(c)),
            }
        }
    }
    // Doc comments arrive with the `///` stripped but the line structure kept.
    dedent(&out)
}

fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let min = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| if l.len() >= min { &l[min..] } else { l.trim_start() })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// The first paragraph, folded to one line, for a table cell.
fn one_line(s: &str) -> String {
    s.split("\n\n").next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_read_as_csharp() {
        assert_eq!(
            signature("Wisp.Response.Text(System.String,System.Int32)"),
            "Text(string, int)"
        );
        assert_eq!(
            signature("Example.Dispatcher.SendAsync(Example.Payload,System.String)"),
            "SendAsync(Payload, string)"
        );
        assert_eq!(signature("Wisp.Ctx.CpuBudgetMs"), "CpuBudgetMs");
        assert_eq!(signature("Wisp.Sdk.Routers.HandlerRouter`1.Boot"), "Boot");
    }

    #[test]
    fn generic_types_render_their_arity() {
        assert_eq!(short_name("Wisp.Abi.Exports`1"), "Exports<T1>");
        assert_eq!(short_name("Example.Pair`2"), "Pair<T1, T2>");
        assert_eq!(short_name("Wisp.Ctx"), "Ctx");
    }

    #[test]
    fn a_generic_argument_comma_does_not_split_the_parameter_list() {
        assert_eq!(
            split_args("System.Collections.Generic.List{System.String},System.Int32"),
            vec!["System.Collections.Generic.List{System.String}", "System.Int32"]
        );
    }

}
