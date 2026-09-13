//! The `wisp` command.
#![forbid(unsafe_code)]

mod dev;

use std::io::IsTerminal as _;

use clap::Parser;
use dev::{dev, DevOpts};
use wisp_cli::app::resolve_limits;
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
        Cmd::Project { directory } => {
            let module = serde_json::to_value(dev::project_module(&directory)?)?;
            println!("{}", serde_json::json!({ "module": module }));
        }
        Cmd::Doctor {
            module,
            guest_memory_bytes,
        } => {
            let bytes = std::fs::read(&module)
                .map_err(|error| anyhow::anyhow!("reading {}: {error}", module.display()))?;
            let report = doctor::analyse(&bytes)?;
            let (limits, limits_from) =
                resolve_limits(guest_memory_bytes.map(|bytes| bytes.get()), &module)?;
            #[cfg(feature = "control")]
            let verdict =
                wisp_runtime::validate_with_memory_limit(&bytes, limits, guest_memory_bytes);
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
                    render::doctor::doctor_json(
                        &report,
                        limits,
                        &limits_from,
                        guest_memory_bytes,
                        rejected,
                        violations
                    )?
                );
            } else {
                print!(
                    "{}",
                    render::doctor(&st, &module.display().to_string(), &report)
                );
                let memory_bytes =
                    guest_memory_bytes.map_or(limits.instance_memory_max, |bytes| bytes.get());
                let memory_from = if guest_memory_bytes.is_some() {
                    "--guest-memory-bytes"
                } else {
                    "limits"
                };
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
                    println!("\n  {}", st.bold("VALIDATOR (wisp-runtime)"));
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

        Cmd::Dev {
            runtime_config_stdin,
            port,
            watch,
            no_watch,
            watch_module_only,
            build,
            no_build,
            init_sandbox,
            poll_ms,
        } => {
            let filter = tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env()?;
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init()
                .map_err(|e| anyhow::anyhow!("initializing local runtime logging: {e}"))?;
            dev(
                &st,
                DevOpts {
                    runtime_configuration: dev::read_runtime_configuration(runtime_config_stdin)?,
                    json: cli.json,
                    port,
                    watch,
                    no_watch,
                    watch_module_only,
                    build,
                    no_build,
                    init_sandbox,
                    poll_ms,
                },
            )
            .await?;
        }

        Cmd::Debug {
            project,
            module,
            debug_endpoint,
            port,
        } => {
            let filter = tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env()?;
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init()
                .map_err(|e| anyhow::anyhow!("initializing debug runtime logging: {e}"))?;
            dev::debug::run(&st, &project, &module, debug_endpoint, port, cli.json).await?;
        }
    }
    Ok(())
}
