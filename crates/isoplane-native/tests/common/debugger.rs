//! Coordinate the DAP driver with the development process's shutdown control.

use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::process::{Child, Command, Output, Stdio};

use crate::common::Dev;

/// Runs the DAP driver, relaying shutdown requests to the development owner.
pub fn drive(mut dev: Option<&mut Dev>, command: &mut Command) -> Output {
    let mut errors = tempfile::tempfile().expect("create the debugger diagnostic capture");
    let mut driver = Driver(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(
                errors
                    .try_clone()
                    .expect("share debugger diagnostic capture"),
            )
            .spawn()
            .expect("start the installed DAP driver"),
    );
    let mut output = Vec::new();
    for line in BufReader::new(driver.0.stdout.take().expect("capture DAP driver output")).lines() {
        let line = line.expect("read DAP driver output");
        if let Some(event) = line.strip_prefix("request-owner-event:") {
            assert!(matches!(
                event,
                "debugger_detached" | "reloaded" | "reload_pending" | "reload_failed"
            ));
            dev.as_mut()
                .expect("session events require a development owner")
                .wait_event(event);
            writeln!(
                driver.0.stdin.as_mut().expect("driver control input"),
                "owner-event:{event}"
            )
            .expect("acknowledge the development event");
        }
        if line == "request-owner-stop" {
            dev.as_mut()
                .expect("owner-stop requires an externally started session")
                .stop();
            driver
                .0
                .stdin
                .as_mut()
                .expect("the DAP driver must have its control input")
                .write_all(b"owner-stopped\n")
                .expect("acknowledge development shutdown to the DAP driver");
        }
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
    let status = driver.0.wait().expect("wait for the DAP driver");
    errors.rewind().expect("rewind debugger diagnostics");
    let mut stderr = Vec::new();
    errors
        .read_to_end(&mut stderr)
        .expect("read debugger diagnostics");
    Output {
        status,
        stdout: output,
        stderr,
    }
}

struct Driver(Child);

impl Drop for Driver {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = self.0.kill() {
                    eprintln!("could not stop the DAP driver: {error}");
                }
                if let Err(error) = self.0.wait() {
                    eprintln!("could not reap the DAP driver: {error}");
                }
            }
            Err(error) => eprintln!("could not inspect the DAP driver: {error}"),
        }
    }
}

/// Runs feature scenarios against one prepared package with private host/session state.
pub fn prepared_acceptance(mode: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let package = isoplane_fixtures::mono_package("debugger");
    let host = root
        .join(".build/clr-anycpu/managed/bin/Isoplane.Cli.Tests/release/Isoplane.Cli.Tests.dll");
    assert!(
        package.is_file() && host.is_file(),
        "run task fixtures:package:debugger test:debugger:host before debugger feature tests"
    );
    let directory = tempfile::tempdir().unwrap();
    crate::common::manifest(directory.path(), "[dev]\ninit_sandbox='runtime-bringup'\n");
    let mut command = std::process::Command::new("dotnet");
    command
        .arg(host)
        .arg("--debug-fixture")
        .arg(directory.path())
        .arg(&package)
        .arg(isoplane_fixtures::mono_debug_runtime_module());
    let mut dev = Dev::spawn(&mut command, std::time::Duration::from_secs(120), true);
    let metadata = dev
        .debugger
        .as_ref()
        .expect("prepared debugger host reports its relay");
    let mut driver = std::process::Command::new(isoplane_fixtures::debugger_python());
    driver
        .arg(root.join("crates/isoplane-native/tests/common/debug_driver.py"))
        .arg(std::env::var_os("ISOPLANE_TEST_CLI_ASSEMBLY").expect("Task selects the prepared CLI"))
        .arg(metadata["endpoint"].as_str().unwrap())
        .arg(format!("{}/{mode}", dev.url.trim_end_matches('/')))
        .arg(root.join(format!("guest-runtime/tests/packages/debugger/{mode}.cs")))
        .arg(dev.pid().to_string())
        .arg(mode)
        .arg(&package);
    let result = drive(Some(&mut dev), &mut driver);
    assert!(
        result.status.success(),
        "prepared debug {mode} failed: {}{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        dev.diagnostics()
    );
    dev.wait_exit();
    assert!(
        !directory.path().join("session").exists(),
        "host shutdown removes its private runtime state"
    );
    assert!(
        package.is_file(),
        "host shutdown preserves the shared package"
    );
}
