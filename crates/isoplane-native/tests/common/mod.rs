//! Shared harness for driving the managed Isoplane CLI as a user would.

use std::process::Command;

fn cli() -> &'static std::path::Path {
    static ASSEMBLY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ASSEMBLY.get_or_init(|| {
        let path = std::env::var_os("ISOPLANE_TEST_CLI_ASSEMBLY")
            .map(std::path::PathBuf::from)
            .expect("run tests through Task to select the prepared CLI");
        assert!(
            path.is_file(),
            "prepared CLI is missing at {}",
            path.display()
        );
        path
    })
}

pub struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(args: &[&str]) -> Out {
    let o = Command::new("dotnet")
        .arg(cli())
        .args(args)
        .output()
        .expect("running the managed CLI");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}
