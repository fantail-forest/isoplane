//! Generate the Rust helper reference from rustdoc.

use anyhow::Result;

use crate::rustdoc::{self, summary, Crate};
use crate::table::{cell, Table};

const PAGE: &str = "sdk-rust.md";
const PACKAGE: &str = "wisp-guest";

/// Modules whose contents are plumbing rather than surface.
///
/// Named, with a reason each, rather than filtered by a heuristic: "which
/// modules does a tenant read" is an editorial judgement, and the honest place
/// for one is a list somebody can argue with.
const SKIP_MODULES: &[(&str, &str)] = &[
    ("abi", "the raw `extern \"C\"` declarations; the typed wrappers are the surface"),
    ("bindings", "re-exported through the modules that own each capability"),
];

pub fn pages() -> Result<Vec<(&'static str, String)>> {
    Ok(vec![(PAGE, render()?)])
}

fn render() -> Result<String> {
    let c = rustdoc::build(&crate::workspace_root(), PACKAGE)?;
    let modules: Vec<String> = c
        .module_items(&c.root)
        .into_iter()
        .filter(|i| c.kind(i).as_deref() == Some("module") && c.is_public(i))
        .map(|i| c.name(&i))
        .collect();
    let mut s = String::new();
    s.push_str("---\nid: sdk-rust\ntitle: Rust SDK\nsidebar_position: 12\n---\n\n");

    // The crate's own `//!` docs open the page. They already begin with an H1.
    let crate_docs = c.docs(&c.root);
    s.push_str(&rustdoc_markdown(&demote(&crate_docs, 0), &modules));
    s.push_str(
        "\n\nEverything below is generated from `guest-sdks/rust`'s own doc comments by \
         `cargo run -p wisp-docs`, which reads rustdoc's JSON output. To change a description \
         here, change the doc comment there.\n",
    );

    // Modules in declaration order, which is the order `lib.rs` chose.
    for id in c.module_items(&c.root) {
        if c.kind(&id).as_deref() != Some("module") || !c.is_public(&id) {
            continue;
        }
        let name = c.name(&id);
        if let Some((_, why)) = SKIP_MODULES.iter().find(|(m, _)| *m == name) {
            s.push_str(&format!("\n{}\n\n*Not documented here: {why}.*\n", module_heading(&name)));
            continue;
        }
        s.push_str(&module_section(&c, &id, &name, &modules)?);
    }

    let missed = c.unrendered.borrow();
    anyhow::ensure!(
        missed.is_empty(),
        "sdk-rust.md: rustdoc JSON used type constructs this reader cannot spell: {missed:?}. \
         Teach `rustdoc::Crate::render_type` about them rather than shipping a page with \
         «placeholders» in the signatures."
    );
    Ok(s)
}

/// A module heading with an **explicit** Docusaurus anchor.
///
/// Slugified from the text, `` `wisp_guest::future` `` becomes
/// `wisp_guestfuture` — unreadable, and not something another page could
/// reasonably be expected to link to. `{#future}` pins a short, stable id
/// that survives any rewording of the heading itself.
fn module_heading(name: &str) -> String {
    format!("## `{PACKAGE}::{name}` {{#{name}}}")
}

fn module_section(c: &Crate, id: &str, name: &str, modules: &[String]) -> Result<String> {
    let mut s = format!("\n{}\n\n", module_heading(name));
    let docs = c.docs(id);
    if !docs.trim().is_empty() {
        s.push_str(&rustdoc_markdown(&demote(&docs, 2), modules));
        s.push('\n');
    }

    let items: Vec<String> = c.module_items(id).into_iter().filter(|i| c.is_public(i)).collect();
    let by = |k: &str| -> Vec<String> {
        items.iter().filter(|i| c.kind(i).as_deref() == Some(k)).cloned().collect()
    };

    for tid in by("trait") {
        s.push_str(&trait_section(c, &tid, modules));
    }
    for sid in by("struct") {
        s.push_str(&struct_section(c, &sid, modules));
    }
    for eid in by("enum") {
        s.push_str(&enum_section(c, &eid, modules));
    }

    let fns = by("function");
    if !fns.is_empty() {
        s.push_str("\n### Functions\n\n");
        let mut t = Table::new(&["Signature", "Meaning"]);
        for f in &fns {
            t.row(&[format!("`{}`", c.signature(f)), cell(&rustdoc_markdown(&summary(&c.docs(f)), modules))]);
        }
        s.push_str(&t.render());
    }

    let macros = by("macro");
    if !macros.is_empty() {
        s.push_str("\n### Macros\n\n");
        let mut t = Table::new(&["Macro", "Meaning"]);
        for m in &macros {
            t.row(&[format!("`{}!`", c.name(m)), cell(&rustdoc_markdown(&summary(&c.docs(m)), modules))]);
        }
        s.push_str(&t.render());
    }

    let consts = by("constant");
    if !consts.is_empty() {
        s.push_str("\n### Constants\n\n");
        let mut t = Table::new(&["Constant", "Meaning"]);
        for k in &consts {
            t.row(&[format!("`{}`", c.name(k)), cell(&rustdoc_markdown(&summary(&c.docs(k)), modules))]);
        }
        s.push_str(&t.render());
    }
    Ok(s)
}

fn trait_section(c: &Crate, id: &str, modules: &[String]) -> String {
    let mut s = format!("\n### `trait {}`\n\n", c.name(id));
    let d = c.docs(id);
    if !d.trim().is_empty() {
        s.push_str(&rustdoc_markdown(&demote(&d, 3), modules));
        s.push('\n');
    }
    let methods: Vec<String> = c
        .get(id)
        .and_then(|i| i["inner"]["trait"]["items"].as_array())
        .map(|a| a.iter().map(|v| v.to_string().trim_matches('"').to_string()).collect())
        .unwrap_or_default();
    let fns: Vec<&String> =
        methods.iter().filter(|m| c.kind(m).as_deref() == Some("function")).collect();
    if !fns.is_empty() {
        let mut t = Table::new(&["Method", "Meaning"]);
        for m in fns {
            t.row(&[format!("`{}`", c.signature(m)), cell(&rustdoc_markdown(&summary(&c.docs(m)), modules))]);
        }
        s.push('\n');
        s.push_str(&t.render());
    }
    s
}

fn struct_section(c: &Crate, id: &str, modules: &[String]) -> String {
    let mut s = format!("\n### `struct {}`\n\n", c.name(id));
    let d = c.docs(id);
    if !d.trim().is_empty() {
        s.push_str(&rustdoc_markdown(&demote(&d, 3), modules));
        s.push('\n');
    }
    let fields: Vec<String> = c
        .get(id)
        .and_then(|i| i["inner"]["struct"]["kind"]["plain"]["fields"].as_array())
        .map(|a| a.iter().map(|v| v.to_string().trim_matches('"').to_string()).collect())
        .unwrap_or_default();
    let pub_fields: Vec<&String> = fields.iter().filter(|f| c.is_public(f)).collect();
    if !pub_fields.is_empty() {
        let mut t = Table::new(&["Field", "Meaning"]);
        for f in pub_fields {
            t.row(&[format!("`{}`", c.name(f)), cell(&rustdoc_markdown(&summary(&c.docs(f)), modules))]);
        }
        s.push('\n');
        s.push_str(&t.render());
    }
    let m = inherent_methods(c, id);
    if !m.is_empty() {
        let mut t = Table::new(&["Method", "Meaning"]);
        for f in &m {
            t.row(&[format!("`{}`", c.signature(f)), cell(&rustdoc_markdown(&summary(&c.docs(f)), modules))]);
        }
        s.push('\n');
        s.push_str(&t.render());
    }
    s
}

fn enum_section(c: &Crate, id: &str, modules: &[String]) -> String {
    let mut s = format!("\n### `enum {}`\n\n", c.name(id));
    let d = c.docs(id);
    if !d.trim().is_empty() {
        s.push_str(&rustdoc_markdown(&demote(&d, 3), modules));
        s.push('\n');
    }
    let variants: Vec<String> = c
        .get(id)
        .and_then(|i| i["inner"]["enum"]["variants"].as_array())
        .map(|a| a.iter().map(|v| v.to_string().trim_matches('"').to_string()).collect())
        .unwrap_or_default();
    if !variants.is_empty() {
        let mut t = Table::new(&["Variant", "Meaning"]);
        for v in &variants {
            t.row(&[format!("`{}`", c.name(v)), cell(&summary(&c.docs(v)))]);
        }
        s.push('\n');
        s.push_str(&t.render());
    }
    let m = inherent_methods(c, id);
    if !m.is_empty() {
        let mut t = Table::new(&["Method", "Meaning"]);
        for f in &m {
            t.row(&[format!("`{}`", c.signature(f)), cell(&rustdoc_markdown(&summary(&c.docs(f)), modules))]);
        }
        s.push('\n');
        s.push_str(&t.render());
    }
    s
}

/// Public methods from `impl Type { .. }` blocks, excluding trait impls.
///
/// A trait impl's methods are the trait's documentation, not the type's;
/// listing them under every implementor is how an API page becomes unreadable.
fn inherent_methods(c: &Crate, id: &str) -> Vec<String> {
    let impls: Vec<String> = c
        .get(id)
        .and_then(|i| {
            let inner = &i["inner"];
            inner["struct"]["impls"].as_array().or_else(|| inner["enum"]["impls"].as_array())
        })
        .map(|a| a.iter().map(|v| v.to_string().trim_matches('"').to_string()).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for imp in impls {
        let Some(item) = c.get(&imp) else { continue };
        if !item["inner"]["impl"]["trait"].is_null() {
            continue;
        }
        let Some(items) = item["inner"]["impl"]["items"].as_array() else { continue };
        for m in items {
            let m = m.to_string().trim_matches('"').to_string();
            if c.kind(&m).as_deref() == Some("function") && !c.docs(&m).trim().is_empty() {
                out.push(m);
            }
        }
    }
    out
}

/// Rewrites rustdoc's markdown dialect into the site's.
///
/// Two differences matter. Intra-doc links are written `[`rt::run`]` with no
/// target — markdown renders that as literal `[rt::run]`, brackets and all —
/// so they become a link to the module's section when one exists and a plain
/// code span otherwise. And rustdoc's doctest fences (`ignore`, `no_run`,
/// `should_panic`) are not languages Prism knows, so they become `rust`.
fn rustdoc_markdown(s: &str, modules: &[String]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("[`") {
        let Some(j) = rest[i..].find("`]") else { break };
        let path = &rest[i + 2..i + j];
        out.push_str(&rest[..i]);
        // `crate::rt`, `rt::run` and `rt` all point at the `rt` section.
        let head = path.trim_start_matches("crate::").split("::").next().unwrap_or("");
        if modules.iter().any(|m| m == head) {
            out.push_str(&format!("[`{path}`](#{head})"));
        } else {
            out.push_str(&format!("`{path}`"));
        }
        rest = &rest[i + j + 2..];
    }
    out.push_str(rest);
    out.lines()
        .map(|l| match l.trim_start() {
            "```ignore" | "```no_run" | "```should_panic" | "```compile_fail" => "```rust",
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shifts markdown headings inside a doc comment so they nest under the
/// section they were spliced into.
///
/// A crate's `//! ## Concurrency` is an H2 in its own document; dropped
/// verbatim under this page's H3 it would break the outline and confuse the
/// sidebar's on-page table of contents.
fn demote(docs: &str, under: usize) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in docs.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && line.starts_with('#') {
            let hashes = line.chars().take_while(|c| *c == '#').count();
            let rest = line[hashes..].trim_start();
            let level = (hashes + under).min(6);
            out.push_str(&format!("{} {rest}\n", "#".repeat(level)));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_from_doc_comments_nest_under_their_section() {
        assert_eq!(demote("## Concurrency\ntext", 2), "#### Concurrency\ntext");
        // Six is the floor; markdown has no h7.
        assert_eq!(demote("###### deep", 3), "###### deep");
        // A `#` inside a fence is a comment, not a heading.
        assert_eq!(demote("```bash\n# a comment\n```", 2), "```bash\n# a comment\n```");
    }

}
