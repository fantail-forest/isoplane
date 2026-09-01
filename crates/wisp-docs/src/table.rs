//! Markdown table building, and the annotation tables that supply the one
//! column a type cannot.

use std::collections::BTreeSet;

/// A GitHub-flavoured markdown table.
pub struct Table {
    headers: Vec<String>,
    /// Per column: `None` left, `Some(true)` right.
    right: Vec<bool>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Table {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            right: vec![false; headers.len()],
            rows: Vec::new(),
        }
    }

    /// Right-align a column. Numbers read wrong ragged-left in a plan table.
    pub fn right_align(mut self, col: usize) -> Self {
        self.right[col] = true;
        self
    }

    pub fn row(&mut self, cells: &[String]) {
        assert_eq!(cells.len(), self.headers.len(), "row width must match the header");
        self.rows.push(cells.to_vec());
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("| {} |\n", self.headers.join(" | ")));
        let rule: Vec<&str> =
            self.right.iter().map(|r| if *r { "---:" } else { "---" }).collect();
        s.push_str(&format!("|{}|\n", rule.join("|")));
        for r in &self.rows {
            s.push_str(&format!("| {} |\n", r.join(" | ")));
        }
        s
    }
}

/// Escapes a cell for a markdown table: a literal `|` would end the cell, and
/// a hard-wrapped sentence would end the row.
///
/// Idempotent. A cell is often assembled from pieces that have already been
/// escaped, and `\\|` renders as a stray backslash *and* still ends the cell —
/// a corruption that looks like a typo rather than like a bug in an escaper.
pub fn cell(s: &str) -> String {
    s.replace("\\|", "|").replace('|', "\\|").replace('\n', " ")
}

/// A tick mark for a boolean policy flag, so the eye finds the exceptions.
pub fn tick(b: bool) -> &'static str {
    if b {
        "✔"
    } else {
        ""
    }
}

/// Thousands separators. `30000` in a table a human reads is a misprint
/// waiting to happen.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Byte counts as the binary unit a reader would have written by hand.
///
/// Exact powers only: anything else prints as bytes rather than as a rounded
/// figure, because a rounded ceiling in a limits table is a ceiling somebody
/// will design against and miss.
pub fn bytes(n: u64) -> String {
    const K: u64 = 1 << 10;
    for (unit, scale) in [("GB", K * K * K), ("MB", K * K), ("KB", K)] {
        if n >= scale && n % scale == 0 {
            return format!("{} {unit}", commas(n / scale));
        }
    }
    format!("{} bytes", commas(n))
}

/// Milliseconds as whole days where they divide evenly, else as milliseconds.
pub fn ms_as_days(ms: u64) -> String {
    const DAY: u64 = 86_400_000;
    if ms % DAY == 0 {
        let d = ms / DAY;
        return format!("{} day{}", commas(d), if d == 1 { "" } else { "s" });
    }
    format!("{} ms", commas(ms))
}

/// A hand-written sentence per row, keyed by an identity that comes from the
/// code.
///
/// This is the half of a reference table a compiler cannot supply: what a trap
/// code *means*, why an import exists. Keeping it here rather than in the page
/// is what lets the rest of the row be generated, and [`Annotations::finish`]
/// is what stops it from rotting — a key with no note, or a note for a key that
/// no longer exists, fails the docs build. Adding a capability is then a change
/// that cannot be made without documenting it.
pub struct Annotations {
    what: &'static str,
    entries: Vec<(&'static str, &'static str)>,
    used: BTreeSet<String>,
    missing: BTreeSet<String>,
}

impl Annotations {
    pub fn new(what: &'static str, entries: &[(&'static str, &'static str)]) -> Self {
        Annotations {
            what,
            entries: entries.to_vec(),
            used: BTreeSet::new(),
            missing: BTreeSet::new(),
        }
    }

    /// The note for `key`, or the empty string with the miss recorded for
    /// [`Annotations::finish`] to report. Collecting misses rather than
    /// bailing on the first one means one run names every undocumented
    /// addition, not just the alphabetically unlucky one.
    pub fn get(&mut self, key: &str) -> String {
        match self.entries.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => {
                self.used.insert(key.to_string());
                cell(v)
            }
            None => {
                self.missing.insert(key.to_string());
                String::new()
            }
        }
    }

    /// Both directions of the check.
    pub fn finish(self) -> anyhow::Result<()> {
        let unused: Vec<&str> = self
            .entries
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| !self.used.contains(*k))
            .collect();
        if self.missing.is_empty() && unused.is_empty() {
            return Ok(());
        }
        let mut msg = String::new();
        if !self.missing.is_empty() {
            let names: Vec<&str> = self.missing.iter().map(|s| s.as_str()).collect();
            msg.push_str(&format!(
                "{}: no documentation for {}. Add a row to the annotation table in \
                 crates/wisp-docs — a new one of these is a change to the platform's \
                 public surface and should not be able to land undocumented.\n",
                self.what,
                names.join(", ")
            ));
        }
        if !unused.is_empty() {
            msg.push_str(&format!(
                "{}: documentation for {} that no longer exists in the code. Delete it.\n",
                self.what,
                unused.join(", ")
            ));
        }
        anyhow::bail!("{}", msg.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_render_as_the_unit_a_human_would_have_typed() {
        assert_eq!(bytes(5 << 20), "5 MB");
        assert_eq!(bytes(10 << 30), "10 GB");
        assert_eq!(bytes(u64::MAX), "18,446,744,073,709,551,615 bytes");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(1_048_576), "1,048,576");
    }

    #[test]
    fn a_pipe_in_a_cell_cannot_end_the_row() {
        assert_eq!(cell("free | paid"), "free \\| paid");
        assert_eq!(cell("two\nlines"), "two lines");
        // Escaping twice must not produce `\\|`, which renders as a stray
        // backslash and still ends the cell.
        assert_eq!(cell(&cell("free | paid")), cell("free | paid"));
    }

    /// The guard that makes an undocumented addition fail rather than render
    /// an empty cell. Proven to fire, in both directions.
    #[test]
    fn an_unannotated_key_and_an_orphan_note_both_fail() {
        let mut a = Annotations::new("test", &[("known", "a note")]);
        assert_eq!(a.get("known"), "a note");
        assert_eq!(a.get("brand_new"), "");
        let err = a.finish().unwrap_err().to_string();
        assert!(err.contains("no documentation for brand_new"), "{err}");

        let mut b = Annotations::new("test", &[("known", "a note"), ("removed", "stale")]);
        let _ = b.get("known");
        let err = b.finish().unwrap_err().to_string();
        assert!(err.contains("no longer exists"), "{err}");
        assert!(err.contains("removed"), "{err}");
    }

    #[test]
    fn a_fully_annotated_table_passes() {
        let mut a = Annotations::new("test", &[("k", "v")]);
        let _ = a.get("k");
        a.finish().unwrap();
    }
}
