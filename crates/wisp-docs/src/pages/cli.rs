//! CLI reference pages generated from the commands parsed by each binary.

use anyhow::Result;
use clap::{Arg, ArgAction, Command, CommandFactory};

use crate::table::{cell, Table};

/// A page, and the command it is a rendering of.
pub struct CliPage {
    pub page: &'static str,
    pub id: &'static str,
    pub title: &'static str,
    /// Preserved verbatim: the sidebar order is an editorial decision that
    /// `sidebars.js` and the reader both depend on.
    pub sidebar_position: u32,
    pub command: fn() -> Command,
    /// The crate manifest, so the build-features table comes from the same
    /// `[features]` section cargo reads rather than from a copy of it.
    pub manifest: &'static str,
}

pub const CLI_PAGES: &[CliPage] = &[
    CliPage {
        page: "cli-wisp.md",
        id: "cli-wisp",
        title: "wisp",
        sidebar_position: 4,
        command: || wisp_cli::cli::Cli::command(),
        manifest: "crates/wisp-cli/Cargo.toml",
    },
    CliPage {
        page: "cli-wispd.md",
        id: "cli-wispd",
        title: "wispd",
        sidebar_position: 5,
        command: || wispd::cli::Args::command(),
        manifest: "crates/wispd/Cargo.toml",
    },
];

/// These pages are emitted in full, so they carry no marker blocks — the whole
/// file is the artifact.
pub fn pages() -> Result<Vec<(&'static str, String)>> {
    CLI_PAGES.iter().map(|p| Ok((p.page, render(p)?))) .collect()
}

fn render(p: &CliPage) -> Result<String> {
    let mut cmd = (p.command)();
    // `build` populates defaults, usage and the implicit `--help`/`--version`;
    // introspecting before it reports a command clap would never parse.
    cmd.build();

    let mut s = String::new();
    s.push_str(&format!(
        "---\nid: {}\ntitle: {}\nsidebar_position: {}\n---\n\n",
        p.id, p.title, p.sidebar_position
    ));
    s.push_str(&format!("# `{}`\n\n", cmd.get_name()));
    s.push_str(&generated_note());
    // The root lists everything, including the options clap propagates.
    s.push_str(&section_body(&cmd, cmd.get_name(), &[])?);
    let skip = inherited(&cmd);
    if !skip.is_empty() {
        let names: Vec<String> = skip.iter().map(|s| format!("`--{s}`")).collect();
        s.push_str(&format!(
            "\n{} are global: they apply to every subcommand below and are not repeated in \
             each table. Every command also takes `--help`.\n",
            names.join(", ")
        ));
    }

    // Subcommands, depth-first.
    let mut sections = Vec::new();
    collect(&cmd, cmd.get_name(), &mut sections);
    for (path, sub) in sections {
        s.push_str(&format!("\n## `{path}`\n\n"));
        s.push_str(&section_body(&sub, &path, &skip)?);
    }

    if let Some(after) = cmd.get_after_long_help().or_else(|| cmd.get_after_help()) {
        s.push_str(&format!("\n{}\n", after.to_string().trim()));
    }
    s.push_str(&build_features(p)?);
    Ok(s)
}

/// The crate's cargo features, read out of its manifest.
///
/// Which features exist and which are on by default is a fact about
/// `Cargo.toml`, and the manifest is the only place that can be wrong about
/// it. What each one *buys* is the crate author's sentence, and it lives where
/// authors already write it: the comment above the feature.
fn build_features(p: &CliPage) -> Result<String> {
    let path = crate::workspace_root().join(p.manifest);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let doc: toml::Value = toml::from_str(&text)?;
    let Some(features) = doc.get("features").and_then(|f| f.as_table()) else {
        return Ok(String::new());
    };
    let default: Vec<String> = features
        .get("default")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let named: Vec<&String> = features.keys().filter(|k| *k != "default").collect();
    if named.is_empty() {
        return Ok(String::new());
    }
    let mut t = Table::new(&["Feature", "Default", "Enables"]);
    for f in &named {
        let enables: Vec<String> = features[*f]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| format!("`{}`", v.trim_start_matches("dep:")))
                    .collect()
            })
            .unwrap_or_default();
        t.row(&[
            format!("`{f}`"),
            if default.contains(f) { "**on**".into() } else { "off".into() },
            cell(&enables.join(", ")),
        ]);
    }
    // Deliberately no worked `cargo build --no-default-features` line. The
    // manifest says which features exist; it does not say that every
    // combination of them compiles, and `wispd`'s does not today. A generated
    // page must not manufacture a claim its source cannot support.
    Ok(format!(
        "\n## Build features\n\nFrom `{}`, the manifest cargo reads.\n\n{}",
        p.manifest,
        t.render(),
    ))
}

fn generated_note() -> String {
    "This page is generated from the command's own definition. To change it, \
     change the doc comments in the source and run `cargo run -p wisp-docs`.\n\n"
        .to_string()
}

/// Every subcommand under `cmd`, with its full invocation path.
fn collect(cmd: &Command, path: &str, out: &mut Vec<(String, Command)>) {
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        let child = format!("{path} {}", sub.get_name());
        out.push((child.clone(), sub.clone()));
        collect(sub, &child, out);
    }
}

/// Options propagated to every subcommand.
fn inherited(root: &Command) -> Vec<String> {
    root.get_arguments()
        .filter(|a| a.is_global_set())
        .filter_map(|a| a.get_long().map(str::to_string))
        .collect()
}

/// Identify clap's built-in help and version actions.
fn is_clap_builtin(a: &Arg) -> bool {
    matches!(a.get_action(), ArgAction::Help | ArgAction::Version)
}

fn section_body(cmd: &Command, path: &str, skip: &[String]) -> Result<String> {
    let mut s = String::new();
    if let Some(about) = cmd.get_long_about().or_else(|| cmd.get_about()) {
        s.push_str(&format!("{}\n\n", about.to_string().trim()));
    }
    s.push_str(&format!("```\n{}\n```\n", usage(cmd, path)));

    let positionals: Vec<&Arg> = cmd.get_arguments().filter(|a| a.is_positional()).collect();
    if !positionals.is_empty() {
        s.push('\n');
        s.push_str(&positional_table(&positionals));
    }

    let options: Vec<&Arg> = cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && !is_clap_builtin(a))
        .filter(|a| a.get_long().is_some_and(|l| !skip.iter().any(|s| s == l)))
        .collect();
    // `help_heading` is where a command's curation lives. Eighty flags in one
    // table is a table nobody reads, and the grouping is a judgement the
    // author of the flag is best placed to make — so it is declared beside
    // the flag rather than reproduced here.
    let mut headings: Vec<Option<&str>> = Vec::new();
    for a in &options {
        let h = a.get_help_heading();
        if !headings.contains(&h) {
            headings.push(h);
        }
    }
    // A command that groups *some* of its flags must group all of them: an
    // ungrouped flag would render in a nameless table above the first heading,
    // where it reads as an oversight nobody owns. Adding a flag to `RunArgs`
    // without a `help_heading` therefore fails here rather than landing on the
    // page unplaced.
    if headings.iter().any(|h| h.is_some()) {
        let unplaced: Vec<&str> = options
            .iter()
            .filter(|a| a.get_help_heading().is_none())
            .filter_map(|a| a.get_long())
            .collect();
        anyhow::ensure!(
            unplaced.is_empty(),
            "`{path}` groups its flags with `help_heading`, but {unplaced:?} have none.              Give them one in the clap definition — the section a flag belongs to is              curation, and it belongs beside the flag."
        );
    }
    let depth_now = path.split_whitespace().count();
    for h in &headings {
        let here: Vec<&Arg> =
            options.iter().filter(|a| a.get_help_heading() == *h).copied().collect();
        if let Some(name) = h {
            // Command headings are always `##`, so a flag group is `###` and
            // a flag's own prose section below is `####`.
            s.push_str(&format!("\n### {name}\n"));
        }
        s.push('\n');
        s.push_str(&option_table(&here));
    }

    let subs: Vec<&Command> =
        cmd.get_subcommands().filter(|c| c.get_name() != "help").collect();
    if !subs.is_empty() {
        s.push('\n');
        s.push_str(&subcommand_table(&subs, path));
    }

    // The paragraphs a table cell could not hold.
    let heading = if headings.iter().any(|h| h.is_some()) { "####" } else { "###" };
    for a in cmd.get_arguments() {
        if is_clap_builtin(a) || a.get_long().is_some_and(|l| skip.iter().any(|s| s == l)) {
            continue;
        }
        let (_, rest) = help_parts(a);
        if rest.is_empty() {
            continue;
        }
        let name = match a.get_long() {
            Some(l) => format!("--{l}"),
            None => a.get_id().as_str().to_uppercase(),
        };
        s.push_str(&format!("\n{heading} `{name}`\n\n{}\n", dedent(&rest)));
    }

    if let Some(after) = cmd.get_after_long_help().or_else(|| cmd.get_after_help()) {
        if depth_now > 1 {
            s.push_str(&format!("\n{}\n", after.to_string().trim()));
        }
    }
    Ok(s)
}

/// Doc comments indent code samples by four spaces; markdown reads four spaces
/// as a code block, which is the intent — but clap has already re-indented
/// relative to the comment, so normalise to exactly four.
fn dedent(s: &str) -> String {
    let min = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    s.lines()
        .map(|l| if l.len() >= min { &l[min..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `wisp deploy [OPTIONS]`, spelled the way clap spells it in `--help`.
fn usage(cmd: &Command, path: &str) -> String {
    let mut c = cmd.clone();
    let rendered = c.render_usage().to_string();
    let body = rendered.trim().strip_prefix("Usage:").unwrap_or(&rendered).trim().to_string();
    // clap already knows the full invocation path for a subcommand it was
    // reached through. Take its first token and splice the rest onto the path
    // only when the two disagree, so `wisp deploy deploy [OPTIONS]` cannot
    // happen in either direction.
    // clap renders the full invocation path for a subcommand it was reached
    // through, so in practice `body` already begins with `path`. Splice only
    // when it does not.
    let root = path.split_whitespace().next().unwrap_or(path);
    if body == path || body.starts_with(&format!("{path} ")) {
        return body;
    }
    match body.split_once(char::is_whitespace) {
        Some((first, rest)) if first == root || first == cmd.get_name() => {
            format!("{path} {rest}")
        }
        _ => format!("{path} {body}"),
    }
}

/// A flag's help, split into the one-line summary a table can hold and the
/// rest.
///
/// A doc comment's later paragraphs are where the real explanation lives —
/// why `--precompile` exists at all, what `--gib-per-core` refuses to alias.
/// Folding those into a table cell produces a cell nobody reads and a table
/// nobody can scan, so the summary goes in the row and the remainder becomes a
/// subsection under the command.
fn help_parts(a: &Arg) -> (String, String) {
    let Some(h) = a.get_long_help().or_else(|| a.get_help()) else {
        return (String::new(), String::new());
    };
    let text = h.to_string();
    let mut paras = text.split("\n\n");
    let first = paras.next().unwrap_or_default();
    let rest: Vec<&str> = paras.collect();
    (one_line(first), rest.join("\n\n").trim().to_string())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn defaults(a: &Arg) -> String {
    // The action first: clap gives a `SetTrue` flag a literal `"false"`
    // default, and printing that in a reference table reads as a value the
    // caller could pass.
    match a.get_action() {
        ArgAction::SetTrue => return "off".into(),
        ArgAction::SetFalse => return "on".into(),
        ArgAction::Help | ArgAction::Version => return String::new(),
        _ => {}
    }
    let d: Vec<String> =
        a.get_default_values().iter().map(|v| format!("`{}`", v.to_string_lossy())).collect();
    if !d.is_empty() {
        return d.join(", ");
    }
    if a.is_required_set() {
        return "**required**".into();
    }
    "none".into()
}

fn value_spelling(a: &Arg) -> String {
    match a.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count | ArgAction::Help
        | ArgAction::Version => String::new(),
        _ => {
            let names: Vec<String> = a
                .get_value_names()
                .map(|n| n.iter().map(|v| format!("<{v}>")).collect())
                .unwrap_or_else(|| vec![format!("<{}>", a.get_id().as_str().to_uppercase())]);
            format!(" {}", names.join(" "))
        }
    }
}

fn option_table(args: &[&Arg]) -> String {
    let show_env = args.iter().any(|a| a.get_env().is_some());
    let headers: Vec<&str> = if show_env {
        vec!["Option", "Env", "Default", "Meaning"]
    } else {
        vec!["Option", "Default", "Meaning"]
    };
    let mut t = Table::new(&headers);
    for a in args {
        let long = a.get_long().expect("filtered to long options");
        let mut name = format!("`--{long}{}`", value_spelling(a));
        if let Some(short) = a.get_short() {
            name = format!("`-{short}`, {name}");
        }
        // An alias is a name a caller may already be using; omitting it from
        // the page is how a working invocation comes to look unsupported.
        let aliases: Vec<String> =
            a.get_all_aliases().unwrap_or_default().iter().map(|x| format!("`--{x}`")).collect();
        if !aliases.is_empty() {
            name = format!("{name} (alias {})", aliases.join(", "));
        }
        if matches!(a.get_action(), ArgAction::Append) {
            name = format!("{name} — repeatable");
        }
        let mut row = vec![name];
        if show_env {
            row.push(
                a.get_env().map(|e| format!("`{}`", e.to_string_lossy())).unwrap_or_default(),
            );
        }
        row.push(defaults(a));
        row.push(cell(&help_parts(a).0));
        t.row(&row);
    }
    t.render()
}

fn positional_table(args: &[&Arg]) -> String {
    let mut t = Table::new(&["Argument", "Required", "Meaning"]);
    for a in args {
        t.row(&[
            format!("`<{}>`", a.get_id().as_str().to_uppercase()),
            if a.is_required_set() { "yes".into() } else { "no".into() },
            cell(&help_parts(a).0),
        ]);
    }
    t.render()
}

fn subcommand_table(subs: &[&Command], path: &str) -> String {
    let mut t = Table::new(&["Command", "Purpose"]);
    for c in subs {
        let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
        t.row(&[
            format!("[`{path} {}`](#{})", c.get_name(), anchor(path, c.get_name())),
            cell(&about),
        ]);
    }
    t.render()
}

/// Derive a heading anchor from command words.
fn anchor(path: &str, leaf: &str) -> String {
    format!("{path} {leaf}").replace(' ', "-").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_lines_name_the_command_once() {
        for p in CLI_PAGES {
            let text = render(p).unwrap();
            let mut in_fence = false;
            for line in text.lines() {
                if line.starts_with("```") {
                    in_fence = !in_fence;
                    continue;
                }
                // Only the invocation lines; a page's example blocks are
                // fenced too, and they legitimately repeat the command.
                if !in_fence || !line.starts_with(p.title) || line.contains("--") {
                    continue;
                }
                let words: Vec<&str> = line.split_whitespace().collect();
                for w in words.windows(2) {
                    assert!(w[0] != w[1], "{}: doubled word in usage `{line}`", p.page);
                }
                assert_eq!(
                    words.iter().filter(|w| **w == p.title).count(),
                    1,
                    "{}: `{}` appears more than once in usage `{line}`",
                    p.page,
                    p.title
                );
            }
        }
    }

    #[test]
    fn every_flag_the_binary_has_reaches_the_page() {
        for p in CLI_PAGES {
            let text = render(p).unwrap();
            let mut cmd = (p.command)();
            cmd.build();
            let mut stack = vec![cmd.clone()];
            let mut checked = 0;
            while let Some(c) = stack.pop() {
                for a in c.get_arguments() {
                    if is_clap_builtin(a) {
                        continue;
                    }
                    if let Some(l) = a.get_long() {
                        assert!(
                            text.contains(&format!("--{l}")),
                            "{}: `--{l}` is in the command but not on the page",
                            p.page
                        );
                        checked += 1;
                    }
                }
                stack.extend(c.get_subcommands().cloned());
            }
            assert!(checked > 3, "{}: read almost no flags; the walk is wrong", p.page);
        }
    }

    /// `onBrokenAnchors: 'throw'` turns a wrong anchor into a failed site
    /// build, so it is worth catching here where the message is legible.
    ///
    /// Checked forwards, against the anchors Docusaurus derives from the
    /// headings; a slug alone cannot recover the original command words.
    #[test]
    fn subcommand_links_point_at_headings_that_exist() {
        for p in CLI_PAGES {
            let text = render(p).unwrap();
            let anchors: Vec<String> = text
                .lines()
                .filter_map(|l| l.strip_prefix("## "))
                .map(|h| h.replace('`', "").trim().replace(' ', "-").to_lowercase())
                .collect();
            let mut links = 0;
            for line in text.lines() {
                let Some(rest) = line.split("](#").nth(1) else { continue };
                let Some(a) = rest.split(')').next() else { continue };
                assert!(
                    anchors.iter().any(|x| x == a),
                    "{}: link #{a} matches no heading; page has {anchors:?}",
                    p.page
                );
                links += 1;
            }
            assert!(links > 0 || p.title == "wispd", "{}: no subcommand links at all", p.page);
        }
    }

    #[test]
    fn frontmatter_is_preserved_so_the_sidebar_still_resolves() {
        for p in CLI_PAGES {
            let text = render(p).unwrap();
            assert!(text.starts_with(&format!("---\nid: {}\n", p.id)), "{}", p.page);
            assert!(text.contains(&format!("sidebar_position: {}", p.sidebar_position)));
        }
    }
}
