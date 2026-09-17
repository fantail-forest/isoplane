//! Shared harness for driving the managed Isoplane CLI as a user would.

use std::process::Command;

fn cli() -> &'static std::path::Path {
    static ASSEMBLY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ASSEMBLY.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let output = Command::new("python3")
            .arg(root.join("scripts/fixture-tools.py"))
            .arg("isoplane-cli")
            .arg(&root)
            .output()
            .expect("build the managed CLI");
        assert!(
            output.status.success(),
            "managed CLI must build: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::path::PathBuf::from(
            String::from_utf8(output.stdout)
                .expect("CLI assembly path is UTF-8")
                .trim(),
        )
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
