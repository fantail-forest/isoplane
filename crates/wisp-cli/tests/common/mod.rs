//! Shared harness for driving the `wisp` binary as a user would.

use std::process::Command;

const WISP: &str = env!("CARGO_BIN_EXE_wisp");

pub struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(args: &[&str]) -> Out {
    let o = Command::new(WISP)
        .args(args)
        .output()
        .expect("running the wisp binary");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}
