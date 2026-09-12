//! Private process owner used by the installed development tools.

#[cfg(target_os = "linux")]
mod linux;

use std::ffi::OsString;

use clap::Parser;

#[derive(Parser)]
struct Options {
    /// Grace after the controlling input closes, before terminating children.
    #[arg(long, default_value_t = 0)]
    grace_ms: u64,
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<OsString>,
}

fn main() {
    let options = Options::parse();
    #[cfg(target_os = "linux")]
    let result = linux::run(
        options.command,
        std::time::Duration::from_millis(options.grace_ms),
    );
    #[cfg(not(target_os = "linux"))]
    let result: std::io::Result<i32> = {
        let _options = options;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "local process ownership is not implemented for this platform",
        ))
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("isoplane: process ownership failed: {error}");
            std::process::exit(1);
        }
    }
}
