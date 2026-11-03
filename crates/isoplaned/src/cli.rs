//! The local host command contract shared by help and argument parsing.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Local host entry point.
#[derive(Parser, Debug)]
#[command(
    name = "isoplaned",
    version,
    about = "Serve and deploy trusted applications on one local host"
)]
pub struct Args {
    /// Operation performed on this host.
    #[command(subcommand)]
    pub command: Command,
}

/// Local operations; no remote deployment transport is implied.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Serve configured applications in the foreground.
    Serve(ServeArgs),
    /// Prepare a portable bundle and select it for the next daemon start.
    Deploy(DeployArgs),
}

/// Foreground host options.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Host configuration file; relative paths resolve beside this file.
    #[arg(long, value_name = "PATH")]
    pub config: PathBuf,
    /// Validate static configuration without opening or changing installation state.
    #[arg(long, conflicts_with = "print_config")]
    pub check: bool,
    /// Print merged nonsecret configuration without starting services.
    #[arg(long)]
    pub print_config: bool,
    /// Use human-readable logs instead of JSON.
    #[arg(long)]
    pub plain_logs: bool,
    /// Full dispatch-event endpoint; flag overrides environment, then file.
    #[arg(long, env = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT")]
    pub otlp_logs_endpoint: Option<crate::logs::OtlpEndpoint>,
    /// Full lifecycle-trace endpoint; flag overrides environment, then file.
    #[arg(long, env = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")]
    pub otlp_traces_endpoint: Option<crate::logs::OtlpEndpoint>,
    /// Stop when the owning CLI closes standard input.
    #[arg(long, hide = true)]
    pub shutdown_on_stdin_close: bool,
}

/// Offline local bundle deployment options.
#[derive(clap::Args, Debug)]
pub struct DeployArgs {
    /// Host configuration file.
    #[arg(long, value_name = "PATH")]
    pub config: PathBuf,
    /// Configured application receiving this bundle.
    #[arg(long)]
    pub app: String,
    /// Portable IL archive built by `isoplane bundle`.
    #[arg(long, value_name = "FILE")]
    pub bundle: PathBuf,
}
