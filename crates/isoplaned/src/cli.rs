//! Local configuration inspection and WebAssembly compilation tools.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name="isoplaned", version, about="Inspect local configuration and compile WebAssembly modules", group=clap::ArgGroup::new("mode").required(true).multiple(false).args(["precompile","check","print_config"]))]
pub struct Args {
    /// Configuration file; otherwise local defaults are used.
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,

    /// Directory used for local compilation artifacts.
    #[arg(long, value_name = "PATH")]
    pub data_dir: Option<std::path::PathBuf>,

    /// Compile a WebAssembly module into the local artifact directory and exit.
    #[arg(long, value_name = "TEMPLATE_WASM")]
    pub precompile: Option<std::path::PathBuf>,

    /// Validate the configuration and exit.
    #[arg(long)]
    pub check: bool,

    /// Print the effective configuration as TOML and exit.
    #[arg(long)]
    pub print_config: bool,
}
