//! The `wisp` command line. The doc comments are the `--help` text.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "wisp",
    version,
    about = "The WISP CLI",
    long_about = "Local project development and WebAssembly validation.",
    after_long_help = "\
## Exit codes\n\n\
| Code | Meaning |\n\
|---|---|\n\
| `0` | Success |\n\
| `1` | Command failed; the reason is on stderr |\n\
| `2` | `wisp doctor` only: the module would be rejected |"
)]
pub struct Cli {
    /// Plain output. Colour is auto-disabled when stdout is not a terminal, so
    /// this is only needed to suppress it at a terminal.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Emit machine-readable JSON instead of the human rendering.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Resolve local project inputs as JSON without building or starting a runtime.
    Project {
        /// Project directory containing wisp.toml.
        #[arg(long, default_value = ".", value_name = "DIR")]
        directory: PathBuf,
    },

    /// Build and run the current project locally; rebuild on source changes.
    Dev {
        /// Read activation-only configuration as one JSON line from the owning CLI.
        #[arg(long, hide = true)]
        runtime_config_stdin: bool,
        /// Local HTTP port; zero selects an available port.
        #[arg(long, default_value_t = wisp_abi::config::DEFAULT_PROXY_PORT)]
        port: u16,
        /// Project directory containing wisp.toml.
        #[arg(long, default_value = ".", value_name = "DIR")]
        watch: PathBuf,
        /// Serve the initial build without watching for changes.
        #[arg(long)]
        no_watch: bool,
        /// Reload only changed module bytes or manifest, for external build tools.
        #[arg(long, requires = "no_build")]
        watch_module_only: bool,
        /// Project build command; overrides [dev].build and build.sh discovery.
        #[arg(long, conflicts_with = "no_build")]
        build: Option<String>,
        /// Execute an existing module without running a build command.
        #[arg(long)]
        no_build: bool,
        /// Initialization policy; overrides [dev].init_sandbox.
        #[arg(long, value_parser = parse_init_sandbox)]
        init_sandbox: Option<wisp_abi::config::InitSandbox>,
        /// Interval between content checks in milliseconds.
        #[arg(long, default_value_t = 300)]
        poll_ms: u64,
    },

    /// Serve one local debug request from a session-owned raw Mono module.
    Debug {
        /// Project directory establishing application and tenant identity.
        #[arg(long)]
        project: PathBuf,
        /// Raw debug module paired with this session's symbols.
        #[arg(long)]
        module: PathBuf,
        /// Exact session-owned IPv4 loopback debugger relay.
        #[arg(long)]
        debug_endpoint: std::net::SocketAddr,
        /// Local HTTP port; zero selects an available port.
        #[arg(long, default_value_t = wisp_abi::config::DEFAULT_PROXY_PORT)]
        port: u16,
    },

    /// Print a module's capability manifest and the validator's verdict on
    /// it, judged against the app's limits. Needs no node. Exits 2 if the
    /// module would be rejected.
    Doctor {
        /// The `.wasm` to inspect.
        module: PathBuf,
        /// Override the application memory ceiling in bytes.
        #[arg(long)]
        guest_memory_bytes: Option<u64>,
    },
}

fn parse_init_sandbox(value: &str) -> Result<wisp_abi::config::InitSandbox, String> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(|e| e.to_string())
}
