//! The `wisp` command.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use wisp_cli::render::Style;
use wisp_cli::{doctor, render};

#[derive(Parser, Debug)]
#[command(name = "wisp", version, about = "The WISP CLI")]
struct Cli {
    #[arg(long, global = true)]
    no_color: bool,

    /// Emit machine-readable JSON instead of the human rendering.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print the capability manifest the validator derives from a module
    Doctor { module: PathBuf },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("wisp: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let color = !cli.no_color && std::io::stdout().is_terminal();
    let st = Style::new(color);

    match cli.cmd {
        Cmd::Doctor { module } => {
            // The only command that needs no node: invariant 4's claim is that
            // this is answerable from the bytes alone.
            let bytes = std::fs::read(&module)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", module.display()))?;
            let report = doctor::analyse(&bytes)?;
            if cli.json {
                println!("{}", doctor_json(&report));
            } else {
                print!(
                    "{}",
                    render::doctor(&st, &module.display().to_string(), &report)
                );
                #[cfg(feature = "control")]
                {
                    // The authority for V1-V4, when it is compiled in.
                    println!("\n  VALIDATOR (wisp-control)");
                    // No application configuration was supplied, so use the default limits.
                    match wisp_control::validate(&bytes, wisp_abi::config::Limits::default()) {
                        Ok(_) => println!("    {} no violations", st.green("✓")),
                        Err(v) => println!("    {} {v:?}", st.red("✗")),
                    }
                }
                #[cfg(not(feature = "control"))]
                println!(
                    "\n  {}",
                    st.dim(
                        "note: this is the manifest derived from the import section. \
                         The authority for rules V1-V4 is `wisp-control::validate`; \
                         rebuild with `--features control` to run it here."
                    )
                );
            }
            if report.would_be_rejected() {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

fn doctor_json(r: &doctor::Report) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "bytes": r.bytes,
        "blake3": r.blake3,
        "rejected": r.would_be_rejected(),
        "capabilities": r.declared.iter().map(|i| serde_json::json!({
            "name": i.name,
            "group": doctor::group_name(i.group),
            "kind": doctor::kind_label(i.kind),
            "sensitive": i.sensitive,
            "init_visible": i.init_visible,
            "wit": i.wit,
        })).collect::<Vec<_>>(),
        "undeclared": r.undeclared.iter().map(|u| format!("{}.{}", u.module, u.field)).collect::<Vec<_>>(),
        "missing_required_exports": r.missing_required_exports,
        "missing_optional_exports": r.missing_optional_exports,
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watcher_notices_the_newest_file_and_ignores_build_output() {
        let dir = std::env::temp_dir().join(format!("wisp-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("src/a.cs"), "x").unwrap();
        let before = newest_mtime(&dir);
        assert!(before > 0);

        // `target/` churns on every build; watching it would reload forever.
        std::fs::write(dir.join("target/huge.bin"), "y").unwrap();
        assert_eq!(
            newest_mtime(&dir),
            before,
            "build output must not trigger a reload"
        );

        assert_eq!(newest_mtime(&dir.join("does-not-exist")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
