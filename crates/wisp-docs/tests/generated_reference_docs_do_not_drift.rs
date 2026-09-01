//! The gate.
//!
//! The reference pages are checked into git because Docusaurus builds from
//! them, which means nothing stops somebody editing a generated table by hand —
//! except this. It regenerates every marked region from the code and fails if
//! the file on disk differs.
//!
//! ```text
//! cargo test -p wisp-docs      # detect drift
//! cargo run  -p wisp-docs      # fix it
//! ```

#[test]
fn the_checked_in_reference_pages_match_what_the_code_says() {
    if let Err(e) = wisp_docs::check() {
        panic!("{e}");
    }
}

/// A generator nobody can run twice is a generator that rewrites the tree on
/// every invocation and makes every diff noise.
#[test]
fn generating_twice_produces_the_same_bytes() {
    for (page, _, once) in wisp_docs::rendered_pages().expect("render") {
        let all = wisp_docs::blocks().expect("blocks");
        let mine: Vec<&wisp_docs::Block> = all.iter().filter(|b| b.page == page).collect();
        let twice = wisp_docs::render_page(&page, &once, &mine).expect("re-render");
        assert_eq!(once, twice, "{page} is not idempotent under regeneration");
    }
}

/// Every page under `reference/` either carries generated regions or is on the
/// list of pages this crate deliberately leaves alone — with a reason.
///
/// Without this, a new reference page could be added and quietly never be
/// covered, which is how the drift this crate exists to stop gets back in.
#[test]
fn every_reference_page_is_either_generated_or_deliberately_not() {
    /// Pages with nothing to derive, and why.
    const PROSE_ONLY: &[(&str, &str)] = &[
        ("index.md", "a table of contents"),
        ("architecture.md", "explanation: the shape of the system, not its constants"),
        (
            "http-api.md",
            "routes and payloads are assembled inside hyper service closures in wispd; \
             deriving them needs a running node or a router type that does not exist yet",
        ),
        (
            "sdk-rust.md",
            "the surface is guest-sdks/rust, a wasm32 crate outside this workspace",
        ),
        (
            "sdk-csharp.md",
            "the surface is guest-sdks/dotnet; deriving it needs a .NET toolchain",
        ),
        (
            "cli-other.md",
            "documents shell scripts and `wisp-acceptance`, which parses its own argv; \
             there is no clap Command and no Rust definition to walk",
        ),
    ];

    let mut generated: Vec<String> =
        wisp_docs::blocks().expect("blocks").iter().map(|b| b.page.to_string()).collect();
    generated.extend(wisp_docs::pages::cli::CLI_PAGES.iter().map(|p| p.page.to_string()));
    let dir = wisp_docs::reference_dir();
    let mut unaccounted = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("reference dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") {
            continue;
        }
        if generated.contains(&name) || PROSE_ONLY.iter().any(|(p, _)| *p == name) {
            continue;
        }
        unaccounted.push(name);
    }
    assert!(
        unaccounted.is_empty(),
        "these reference pages are neither generated nor listed as prose-only: {unaccounted:?}. \
         Add a builder in crates/wisp-docs/src/pages, or add the page to PROSE_ONLY with the \
         reason it cannot be derived."
    );

    // And the list may not name a page that no longer exists.
    for (p, _) in PROSE_ONLY {
        assert!(dir.join(p).exists(), "PROSE_ONLY names {p}, which is not in {}", dir.display());
    }
}
