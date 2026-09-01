//! Generate capability reference tables from the canonical import definitions.

use anyhow::Result;
use wisp_abi::imports::{
    self, BufSpec, Group, ImportDef, ImportKind, Sig, Ty, HOST_MODULE, IMPORTS,
};

use crate::table::{cell, tick, Table};
use crate::Block;

const PAGE: &str = "capabilities.md";

/// Reference sections, checked against the actual import definitions.
const SECTIONS: &[(&str, &[Group])] = &[
    ("imports-core", &[Group::Core]),
    ("imports-observe", &[Group::Observe]),
    ("imports-clock-entropy", &[Group::Clock, Group::Entropy]),
    ("imports-http", &[Group::Http]),
    ("imports-response", &[Group::Response]),
    ("imports-ctx", &[Group::Ctx]),
];



pub fn blocks() -> Result<Vec<Block>> {
    // The partition check, before anything is rendered: every group in exactly
    // one section, and no section naming a group that no longer exists.
    for import in IMPORTS {
        let g = import.group;
        let n = SECTIONS.iter().filter(|(_, gs)| gs.contains(&g)).count();
        if n != 1 {
            anyhow::bail!(
                "capabilities.md: group {:?} appears in {n} sections, expected exactly 1. \
                 Add it to SECTIONS in crates/wisp-docs — an import in no section is an \
                 undocumented capability.",
                g
            );
        }
    }
    for (id, gs) in SECTIONS {
        for g in *gs {
            if !IMPORTS.iter().any(|import| import.group == *g) {
                anyhow::bail!("capabilities.md: section {id} names a group with no imports");
            }
        }
    }

    let mut out = vec![
        Block::new(PAGE, "import-preamble", preamble()),
        Block::new(PAGE, "import-column-key", column_key()),
    ];
    for (id, groups) in SECTIONS {
        out.push(Block::new(PAGE, id, group_table(groups)?));
    }
    out.push(Block::new(PAGE, "import-structural-guarantees", structural_guarantees()));
    Ok(out)
}

fn preamble() -> String {
    format!(
        "The `{HOST_MODULE}` import definitions are recorded in \
         `wisp_abi::imports::IMPORTS`. The tables list **{}** definitions; {} carry the \
         initialization flag and {} carry the sensitive flag. PAL imports can delegate to \
         host capabilities, so inspect both direct imports and PAL declarations with \
         `wisp doctor`.",
        IMPORTS.len(),
        imports::init_visible().count(),
        imports::sensitive().count(),
    )
}

/// Explains the generated capability columns.
fn column_key() -> String {
    let mut t = Table::new(&["Column", "Meaning"]);
    t.row(&[
        "**Kind**".into(),
        cell(&format!(
            "`{}` returns a result directly. `{}` returns a future handle for later polling.",
            kind_word(ImportKind::Sync),
            kind_word(ImportKind::Future),
        )),
    ]);
    t.row(&[
        "**Init**".into(),
        cell("Reports the import definition's `init_visible` flag. The preparation sandbox's policy determines the calls available in a particular initialization context."),
    ]);
    t.row(&[
        "**Sens**".into(),
        cell("Marks potentially sensitive results. Guest memory can retain copies; this flag does not remove them from captured state."),
    ]);
    t.render()
}

fn kind_word(k: ImportKind) -> &'static str {
    match k {
        ImportKind::Sync => "sync",
        ImportKind::Future => "future",
    }
}


fn kind_cell(k: ImportKind) -> String {
    match k {
        ImportKind::Sync => "sync".into(),
        ImportKind::Future => "future".into(),
    }
}

fn ty(t: Ty) -> &'static str {
    match t {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    }
}

/// The core-wasm signature, with the buffer pair named.
///
/// `(i32, i32)` alone is ambiguous — it is an **in** buffer for `abort` and an
/// **out** buffer for `entropy_fill` — so [`BufSpec`] is folded in here rather
/// than left for the reader to infer, which is the same reason `wisp-abi`
/// declares it at all.
fn signature(def: &ImportDef) -> String {
    let Sig { params, results } = def.sig;
    let mut names: Vec<String> = params.iter().map(|t| ty(*t).to_string()).collect();
    match def.buf {
        BufSpec::None => {}
        BufSpec::In(i) => {
            names[i as usize] = "ptr".into();
            names[i as usize + 1] = "len".into();
        }
        BufSpec::Out(i) => {
            names[i as usize] = "ptr".into();
            names[i as usize + 1] = "cap".into();
        }
    }
    let res = match results.len() {
        0 => "()".to_string(),
        _ => results.iter().map(|t| ty(*t).to_string()).collect::<Vec<_>>().join(", "),
    };
    format!("`({}) -> {res}`", names.join(", "))
}

fn buf_cell(def: &ImportDef) -> &'static str {
    match def.buf {
        BufSpec::None => "—",
        BufSpec::In(_) => "guest → host",
        BufSpec::Out(_) => "**host → guest**",
    }
}

fn group_table(groups: &[Group]) -> Result<String> {
    let mut t = Table::new(&["Import", "Signature", "Buffer", "Kind", "Init", "Sens", "What it is for"]);
    // `IMPORTS` order, filtered — the declaration order in the table is itself
    // meaningful (core first, then the capability groups), so it is preserved.
    for def in IMPORTS.iter().filter(|d| groups.contains(&d.group)) {
        t.row(&[
            format!("`{}`", def.name),
            signature(def),
            buf_cell(def).to_string(),
            kind_cell(def.kind),
            tick(def.init_visible).to_string(),
            tick(def.sensitive).to_string(),
            cell(def.why),
        ]);
        has_a_reason(def)?;
    }
    Ok(t.render())
}

/// The `imp!` macro makes `why` mandatory, so the only way to reach the docs
/// with an undocumented capability is to pass the empty string on purpose.
fn has_a_reason(def: &ImportDef) -> Result<()> {
    if def.why.trim().is_empty() {
        anyhow::bail!(
            "capabilities.md: import `{}` has an empty `why` in wisp-abi. A capability \
             nobody can explain is a capability nobody reviewed.",
            def.name
        );
    }
    Ok(())
}

/// Summarizes selected metadata from the canonical capability definitions.
fn structural_guarantees() -> String {
    let out: Vec<String> = IMPORTS
        .iter()
        .filter(|i| matches!(i.buf, BufSpec::Out(_)))
        .map(|i| format!("`{}`", i.name))
        .collect();
    let init_sensitive: Vec<String> = imports::init_visible()
        .filter(|i| i.sensitive)
        .map(|i| format!("`{}`", i.name))
        .collect();
    let init: Vec<String> = imports::init_visible().map(|i| format!("`{}`", i.name)).collect();

    format!(
        "Initialization metadata flags **{}** imports: {}. Sensitive imports among them: {}.\n\
         Imports with a declared output buffer: {} \u{2014} {}.\n\
         These lists describe metadata. They do not state what guest code retains in memory; \
         returned values and values derived from them can enter captured guest state.\n\
",
        init.len(),
        init.join(", "),
        init_sensitive.join(", "),
        out.len(),
        out.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_in_the_preamble_is_the_length_of_the_table() {
        let p = preamble();
        assert!(p.contains(&format!("**{}**", IMPORTS.len())), "{p}");
        assert!(
            p.contains(&format!("{} carry the", imports::init_visible().count())),
            "the init-visible count must be counted, not typed\n{p}"
        );
    }

    #[test]
    fn every_import_lands_in_exactly_one_section() {
        blocks().unwrap();
        let mut seen = 0;
        for (_, groups) in SECTIONS {
            seen += IMPORTS.iter().filter(|d| groups.contains(&d.group)).count();
        }
        assert_eq!(seen, IMPORTS.len(), "an import fell out of every section");
    }

    /// Proven to fire.
    #[test]
    fn an_import_with_an_empty_reason_is_refused() {
        let blank = ImportDef { why: "  ", ..*imports::lookup("http_fetch").unwrap() };
        let err = has_a_reason(&blank).unwrap_err().to_string();
        assert!(err.contains("nobody reviewed"), "{err}");
        for def in IMPORTS {
            has_a_reason(def).expect("every shipped import explains itself");
        }
    }

    #[test]
    fn the_purpose_column_is_the_definitions_own_sentence() {
        let t = group_table(&[Group::Http]).unwrap();
        assert!(t.contains(imports::lookup("http_fetch").unwrap().why), "{t}");
    }

    #[test]
    fn the_signature_names_the_buffer_and_says_which_way_it_flows() {
        let entropy = imports::lookup("entropy_fill").unwrap();
        assert_eq!(signature(entropy), "`(ptr, cap) -> ()`");
        assert_eq!(buf_cell(entropy), "**host → guest**");
        let abort = imports::lookup("abort").unwrap();
        assert_eq!(signature(abort), "`(ptr, len) -> ()`");
        assert_eq!(buf_cell(abort), "guest → host");
        // The shape that looks identical and carries no buffer at all.
        let read = imports::lookup("http_body_read").unwrap();
        assert_eq!(signature(read), "`(i32, i32) -> i64`");
        assert_eq!(buf_cell(read), "—");
    }

    #[test]
    fn the_metadata_lists_name_the_declared_output_buffers() {
        let s = structural_guarantees();
        assert!(s.contains("`init_hash_seed`, `init_config_get`, `entropy_fill`"), "{s}");
    }
}
