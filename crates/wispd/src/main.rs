//! Local configuration inspection tools.

use clap::Parser;

use wispd::config::NodeConfig;

#[derive(Parser, Debug)]
#[command(name="wispd", version, about="Inspect local configuration", group=clap::ArgGroup::new("mode").required(true).multiple(false).args(["check","print_config"]))]
struct Args {
    /// Configuration file; otherwise local defaults are used.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Validate the configuration and exit.
    #[arg(long)]
    check: bool,

    /// Print the effective configuration as TOML and exit.
    #[arg(long)]
    print_config: bool,
}

fn effective_config(args: &Args) -> anyhow::Result<NodeConfig> {
    let cfg = match &args.config {
        Some(p) => NodeConfig::from_toml(&std::fs::read_to_string(p)?)?,
        None => NodeConfig::dev_default(),
    };
    Ok(cfg)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = effective_config(&args)?;

    if args.print_config {
        println!("{}", toml::to_string_pretty(&cfg)?);
        return Ok(());
    }
    if args.check {
        return match cfg.validate() {
            Ok(()) => {
                println!("ok: {} cells, {} apps", cfg.cells.len(), cfg.apps.len());
                Ok(())
            }
            Err(e) => {
                eprintln!("wispd: refusing this configuration: {e}");
                std::process::exit(1);
            }
        };
    }
    Ok(())
}
