//! The `wisp` command.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use clap::Parser;
use wisp_cli::cli::{Cli, Cmd};
use wisp_cli::render::Style;
use wisp_cli::{doctor, render};

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
        Cmd::Doctor {
            module,
            guest_memory_bytes,
        } => {
            let bytes = std::fs::read(&module)
                .map_err(|error| anyhow::anyhow!("reading {}: {error}", module.display()))?;
            let report = doctor::analyse(&bytes)?;
            let (limits, limits_from) = resolve_limits(guest_memory_bytes, &module)?;
            #[cfg(feature = "control")]
            let verdict = wisp_control::validate(&bytes, limits);
            #[cfg(feature = "control")]
            let rejected = verdict.is_err();
            #[cfg(not(feature = "control"))]
            let rejected = report.would_be_rejected();
            if cli.json {
                #[cfg(feature = "control")]
                let violations = verdict.as_ref().err().map(Vec::as_slice).unwrap_or(&[]);
                #[cfg(not(feature = "control"))]
                let violations: &[()] = &[];
                println!(
                    "{}",
                    doctor_json(&report, limits, &limits_from, rejected, violations)
                );
            } else {
                print!(
                    "{}",
                    render::doctor(&st, &module.display().to_string(), &report)
                );
                let memory_bytes = limits.instance_memory_max;
                let memory_from = limits_from.as_str();
                println!(
                    "    {}",
                    st.dim(&format!("numeric limits from {limits_from}"))
                );
                println!(
                    "    {}",
                    st.dim(&format!(
                        "guest memory ceiling: {memory_bytes} bytes ({memory_from})"
                    ))
                );
                #[cfg(feature = "control")]
                {
                    println!("\n  {}", st.bold("VALIDATOR (wisp-control)"));
                    match &verdict {
                        Ok(_) => println!("    {} no violations", st.green("✓")),
                        Err(violations) => {
                            for violation in violations {
                                let subject = violation
                                    .subject
                                    .as_ref()
                                    .map(|value| format!(" {value}"))
                                    .unwrap_or_default();
                                println!(
                                    "    {} {:?}{subject} — {}",
                                    st.red("✗"),
                                    violation.code,
                                    violation.detail
                                );
                            }
                        }
                    }
                }
                #[cfg(not(feature = "control"))]
                println!("\n  {}", st.dim("note: the full validator is disabled in this build; drop --no-default-features to run it here."));
            }
            if rejected {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

fn resolve_limits(
    memory_flag: Option<u64>,
    module: &std::path::Path,
) -> anyhow::Result<(wisp_abi::config::Limits, String)> {
    let from_module = module
        .canonicalize()
        .ok()
        .and_then(|module| module.parent().map(std::path::Path::to_path_buf));
    let mut selected = None;
    for start in [from_module, std::env::current_dir().ok()]
        .into_iter()
        .flatten()
    {
        let mut directory = Some(start.as_path());
        while let Some(path) = directory {
            if let Some((manifest, source)) = wisp_cli::app::CliManifest::load(path)? {
                selected = Some(match manifest.limits {
                    Some(limits) => (limits, format!("limits in {}", source.display())),
                    None => (
                        wisp_abi::config::Limits::default(),
                        format!(
                            "{} declares no `limits`; using the default",
                            source.display()
                        ),
                    ),
                });
                break;
            }
            directory = path.parent();
        }
        if selected.is_some() {
            break;
        }
    }
    let (mut limits, source) = selected.unwrap_or_else(|| {
        (
            wisp_abi::config::Limits::default(),
            "no wisp.toml found; using the default".into(),
        )
    });
    if let Some(memory) = memory_flag {
        limits.instance_memory_max = memory;
        return Ok((
            limits,
            format!("--guest-memory-bytes; other limits from {source}"),
        ));
    }
    Ok((limits, source))
}

/// A validator violation as JSON.
///
/// Behind a trait so the shape of `--json` does not depend on whether
/// `wisp-control` is linked in: without it the caller passes an empty `&[()]`
/// and the key is present and empty, rather than absent.
trait AsViolationJson {
    fn as_json(&self) -> serde_json::Value;
}

impl AsViolationJson for () {
    fn as_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

#[cfg(feature = "control")]
impl AsViolationJson for wisp_control::Violation {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": format!("{:?}", self.code),
            "subject": self.subject,
            "detail": self.detail,
        })
    }
}

fn doctor_json<V: AsViolationJson>(
    r: &doctor::Report,
    limits: wisp_abi::config::Limits,
    limits_from: &str,
    rejected: bool,
    violations: &[V],
) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "bytes": r.bytes,
        "blake3": r.blake3,
        "limits": limits,
"guest_memory_bytes": limits.instance_memory_max,
        "limits_from": limits_from,
        "validator": cfg!(feature = "control"),
        "violations": violations.iter().map(AsViolationJson::as_json).collect::<Vec<_>>(),
        "rejected": rejected,
        "capabilities": r.declared.iter().map(|i| serde_json::json!({
            "name": i.name,
            "group": doctor::group_name(i.group),
            "kind": doctor::kind_label(i.kind),
            "sensitive": i.sensitive,
            "init_visible": i.init_visible,
        })).collect::<Vec<_>>(),
        "undeclared": r.undeclared.iter().map(|u| format!("{}.{}", u.module, u.field)).collect::<Vec<_>>(),
        // The PAL entries are declared too, and what a consumer needs from
        // them is which `wisp` capability each one actually reaches.
        "pal": r.pal.iter().map(|p| serde_json::json!({
            "name": p.full_name(),
            "delegates_to": p.disposition.capability(),
            "traps": !p.disposition.is_delegated(),
        })).collect::<Vec<_>>(),
        "missing_required_exports": r.missing_required_exports,
        "missing_optional_exports": r.missing_optional_exports,
    }))
    .unwrap()
}

// ---------------------------------------------------------------------------
// `wisp dev`
// ---------------------------------------------------------------------------

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
