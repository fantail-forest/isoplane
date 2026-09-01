//! The `wisp` command line, as types.
//!
//! Here rather than in `main.rs` so that exactly one definition of the command
//! surface exists: `main.rs` parses it, and `wisp-docs` walks the same
//! [`clap::Command`] to emit `reference/cli-wisp.md`. The doc comments below
//! are what a tenant reads in `--help` *and* on that page — write them for
//! both.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "wisp",
    version,
    about = "The WISP CLI",
    long_about = "WebAssembly validation.",
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

    /// Inspect a WebAssembly module's host interface and validation status.
    Doctor {
        /// The `.wasm` to inspect.
        module: PathBuf,
        /// Override the application memory ceiling in bytes.
        #[arg(long)]
        guest_memory_bytes: Option<u64>,
    },
}
