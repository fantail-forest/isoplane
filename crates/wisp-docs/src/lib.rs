//! Reference tables are generated from the definitions the runtime reads.
//! Checked-in pages are verified by [`check`] for drift.
//!
//! ## What is generated and what is not
//!
//! Only the *facts* — names, numbers, signatures, enum values, flags, and the
//! order they come in. The prose around each table is hand-written and the
//! generator never touches it, because a generator that flattens good
//! explanatory writing into a machine dump is a bad trade.
//!
//! Where a table needs a sentence per row that no type carries — "why does this
//! import exist" — that sentence lives in an annotation table in this crate,
//! keyed by the identity from the code, and **a key with no annotation is an
//! error**. Adding an import, a trap code or a violation code therefore fails
//! the docs build until somebody writes the row. That is the only thing here
//! that is still hand-written, and it is the half a compiler cannot supply.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub mod pages;
pub mod rustdoc;
pub mod table;

/// One generated region: `body` replaces whatever currently sits between the
/// markers named `id` on `page`.
#[derive(Debug, Clone)]
pub struct Block {
    /// File name under `docs/userdocs/docs/reference/`.
    pub page: &'static str,
    pub id: &'static str,
    pub body: String,
}

impl Block {
    pub fn new(page: &'static str, id: &'static str, body: impl Into<String>) -> Self {
        Block {
            page,
            id,
            body: body.into(),
        }
    }
}

/// Every block the generator knows how to emit, in page order.
///
/// Fallible because most of the builders below cross-check themselves — an
/// import with no annotation, a metric series in no documented section, a
/// marker for a table nobody generates. Those are the checks that make adding
/// a capability fail here rather than pass silently.
pub fn blocks() -> anyhow::Result<Vec<Block>> {
    let mut out = Vec::new();
    out.extend(pages::capabilities::blocks()?);
    out.extend(pages::validator::blocks()?);
    out.extend(pages::guest_abi::blocks()?);
    out.extend(pages::wisp_toml::blocks()?);
    out.extend(pages::metrics::blocks()?);
    Ok(out)
}

/// MDX, not HTML, comments.
///
/// Docusaurus 3 parses `.md` under the MDX compiler unless told otherwise, and
/// this site does not tell it otherwise — so `<!-- ... -->` is a build error
/// here, while `{/* ... */}` is an expression that renders nothing.
fn begin_marker(id: &str) -> String {
    format!("{{/* @generated:begin {id} */}}")
}

fn end_marker(id: &str) -> String {
    format!("{{/* @generated:end {id} */}}")
}

/// Splices every block for one page into its text.
///
/// Refuses rather than guesses: a missing marker pair, a duplicated one, or a
/// marked region nothing generates are all errors, because each of them is a
/// page that would silently stop being checked.
pub fn render_page(page: &str, text: &str, blocks: &[&Block]) -> anyhow::Result<String> {
    let mut out = text.to_string();
    for b in blocks {
        let (begin, end) = (begin_marker(b.id), end_marker(b.id));
        let count = out.matches(&begin).count();
        if count != 1 {
            anyhow::bail!(
                "{page}: expected exactly one `{begin}`, found {count}. \
                 Add the marker pair around the table this block generates."
            );
        }
        let ecount = out.matches(&end).count();
        if ecount != 1 {
            anyhow::bail!("{page}: expected exactly one `{end}`, found {ecount}");
        }
        let bstart = out.find(&begin).expect("counted above");
        let bend = bstart + begin.len();
        let estart = out.find(&end).expect("counted above");
        if estart < bend {
            anyhow::bail!("{page}: `{end}` appears before `{begin}`");
        }
        let body = b.body.trim_end();
        out = format!("{}\n{}\n{}", &out[..bend], body, &out[estart..]);
    }

    first_paragraph_is_prose(page, &out)?;

    // A marker the generator does not know about is a region that looks
    // generated, reads as authoritative, and is checked by nothing.
    let known: BTreeSet<&str> = blocks.iter().map(|b| b.id).collect();
    for id in marker_ids(&out) {
        if !known.contains(id.as_str()) {
            anyhow::bail!(
                "{page}: `@generated:begin {id}` has no builder in wisp-docs. \
                 Either add one or delete the markers — a marked region nothing \
                 generates is checked by nothing."
            );
        }
    }
    Ok(out)
}

/// Docusaurus takes a page's `description` meta tag from its first paragraph,
/// and takes it from the *source*, markers and all — so a page that opens on a
/// generated block ships `{/ @generated:begin … /}` as its search-result
/// summary and its link preview. Invisible on the page itself, which is exactly
/// why it needs a check.
fn first_paragraph_is_prose(page: &str, text: &str) -> anyhow::Result<()> {
    let mut in_frontmatter = false;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if i == 0 && t == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if t == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("{/* @generated:begin ") {
            anyhow::bail!(
                "{page}: the first paragraph is a generated block. Docusaurus takes the page's \
                 description meta tag from the source of the first paragraph, so this ships the \
                 marker text as the page's search summary. Put a hand-written sentence first."
            );
        }
        return Ok(());
    }
    Ok(())
}

fn marker_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("{/* @generated:begin ") {
            if let Some(id) = rest.strip_suffix(" */}") {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

/// The repository root, found from this crate's manifest rather than the
/// working directory.
pub fn workspace_root() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    p.canonicalize().unwrap_or(p)
}

/// `docs/userdocs/docs/reference/`, found from this crate's manifest rather
/// than the working directory, so the generator behaves the same however it is
/// invoked.
pub fn reference_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/userdocs/docs/reference")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/userdocs/docs/reference")
        })
}

/// The regenerated text of every page this crate produces: `(page, on disk,
/// from code)`.
///
/// Two kinds of page. A **whole** page is emitted from first byte to last and
/// has no markers at all — the `cli-*.md` pages, where clap already holds the
/// entire surface. A **marked** page keeps its hand-written prose and has only
/// its tables replaced.
pub fn rendered_pages() -> anyhow::Result<Vec<(String, String, String)>> {
    let dir = reference_dir();
    let mut out = Vec::new();
    for (page, rendered) in pages::cli::pages()?
        .into_iter()
        .chain(pages::sdk_rust::pages()?)
        .chain(pages::sdk_csharp::pages()?)
    {
        let path = dir.join(page);
        let current = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        out.push((page.to_string(), current, rendered));
    }
    out.extend(rendered_marked_pages()?);
    Ok(out)
}

fn rendered_marked_pages() -> anyhow::Result<Vec<(String, String, String)>> {
    let all = blocks()?;
    let mut pages: Vec<&'static str> = all.iter().map(|b| b.page).collect();
    pages.sort_unstable();
    pages.dedup();

    let dir = reference_dir();
    let mut out = Vec::new();
    for page in pages {
        let path = dir.join(page);
        let current = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let mine: Vec<&Block> = all.iter().filter(|b| b.page == page).collect();
        let rendered = render_page(page, &current, &mine)?;
        out.push((page.to_string(), current, rendered));
    }
    Ok(out)
}

/// Rewrites the checked-in pages. Returns the ones that changed.
pub fn write() -> anyhow::Result<Vec<String>> {
    let dir = reference_dir();
    let mut changed = Vec::new();
    for (page, current, rendered) in rendered_pages()? {
        if current != rendered {
            std::fs::write(dir.join(&page), &rendered)?;
            changed.push(page);
        }
    }
    Ok(changed)
}

/// The drift check. `Ok(())` when every page on disk equals what the code says
/// it should be; otherwise an error naming the pages and showing the first
/// differing lines.
pub fn check() -> anyhow::Result<()> {
    let mut report = String::new();
    for (page, current, rendered) in rendered_pages()? {
        if current != rendered {
            report.push_str(&format!(
                "\n--- {page} ---\n{}",
                first_diff(&current, &rendered)
            ));
        }
    }
    if report.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "generated reference documentation is stale.\n\
         The pages below no longer match the definitions they document.\n\
         Regenerate with:  cargo run -p wisp-docs\n{report}"
    );
}

/// A short, readable diff: the first run of differing lines and a count of the
/// rest. Enough to see *what* moved without printing a whole page into a test
/// failure.
fn first_diff(a: &str, b: &str) -> String {
    let (av, bv): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let mut i = 0;
    while i < av.len() && i < bv.len() && av[i] == bv[i] {
        i += 1;
    }
    let mut s = String::new();
    let mut shown = 0;
    let mut j = i;
    while j < av.len().max(bv.len()) && shown < 12 {
        let l = av.get(j).copied().unwrap_or("<end of file>");
        let r = bv.get(j).copied().unwrap_or("<end of file>");
        if l != r {
            s.push_str(&format!(
                "  line {}:\n    on disk:   {l}\n    from code: {r}\n",
                j + 1
            ));
            shown += 1;
        }
        j += 1;
    }
    let total = (i..av.len().max(bv.len()))
        .filter(|&k| av.get(k).copied().unwrap_or("") != bv.get(k).copied().unwrap_or(""))
        .count();
    if total > shown {
        s.push_str(&format!(
            "  … and {} further differing lines\n",
            total - shown
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_replaces_only_what_is_between_its_markers() {
        let page =
            "before\n\n{/* @generated:begin t */}\nstale\n{/* @generated:end t */}\n\nafter\n";
        let b = Block::new("p.md", "t", "fresh");
        let out = render_page("p.md", page, &[&b]).unwrap();
        assert_eq!(
            out,
            "before\n\n{/* @generated:begin t */}\nfresh\n{/* @generated:end t */}\n\nafter\n"
        );
    }

    #[test]
    fn a_missing_marker_is_an_error_rather_than_a_silent_skip() {
        let b = Block::new("p.md", "t", "fresh");
        let err = render_page("p.md", "no markers here\n", &[&b])
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected exactly one"), "{err}");
    }

    #[test]
    fn a_marked_region_with_no_builder_is_an_error() {
        let page = "Prose.\n\n{/* @generated:begin orphan */}\nx\n{/* @generated:end orphan */}\n";
        let err = render_page("p.md", page, &[]).unwrap_err().to_string();
        assert!(err.contains("no builder"), "{err}");
    }

    /// Proven to fire, on the shape that actually shipped a marker into a
    /// page's search summary.
    #[test]
    fn a_page_that_opens_on_a_generated_block_is_refused() {
        let page = "---\nid: m\n---\n\n# Metrics\n\n{/* @generated:begin t */}\nx\n{/* @generated:end t */}\n";
        let b = Block::new("m.md", "t", "x");
        let err = render_page("m.md", page, &[&b]).unwrap_err().to_string();
        assert!(err.contains("description meta tag"), "{err}");

        let ok = "---\nid: m\n---\n\n# Metrics\n\nA sentence.\n\n{/* @generated:begin t */}\nold\n{/* @generated:end t */}\n";
        render_page("m.md", ok, &[&b]).unwrap();
    }

    #[test]
    fn rendering_is_idempotent() {
        let page = "Prose.\n\n{/* @generated:begin t */}\nold\n{/* @generated:end t */}\n";
        let b = Block::new("p.md", "t", "new\nlines");
        let once = render_page("p.md", page, &[&b]).unwrap();
        let twice = render_page("p.md", &once, &[&b]).unwrap();
        assert_eq!(once, twice);
    }
}
