//! Coordinate the DAP driver with the development process's shutdown control.

use std::io::{BufRead, BufReader, Read, Seek, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::common::Dev;

/// Runs the DAP driver, relaying shutdown requests to the development owner.
pub fn drive(mut dev: Option<&mut Dev>, command: &mut Command) -> Output {
    let mut errors = tempfile::tempfile().expect("create the debugger diagnostic capture");
    #[cfg(unix)]
    command.process_group(0);
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
    let stdout = driver.0.stdout.take().expect("capture DAP driver output");
    let (sender, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                // The owner stopped waiting after the driver exceeded its deadline.
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut output = Vec::new();
    loop {
        let line = match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => line.expect("read DAP driver output"),
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                drop(driver);
                errors.rewind().expect("rewind debugger diagnostics");
                let mut diagnostics = String::new();
                errors
                    .read_to_string(&mut diagnostics)
                    .expect("read debugger diagnostics");
                panic!(
                    "debugger driver exceeded its deadline\n{}\n{diagnostics}",
                    String::from_utf8_lossy(&output)
                );
            }
        };
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
        crate::common::terminate_test_tree(self.0.id(), matches!(self.0.try_wait(), Ok(Some(_))));
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

fn repository() -> &'static std::path::Path {
    // Roslyn misparses compiler output paths in Windows' extended UNC syntax.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/isoplane-native has a grandparent")
}

/// Loads a prepared compiler baseline without paying for a guest invocation.
pub fn compiler_acceptance() {
    let root = repository();
    let managed = root.join(".build/clr-anycpu/managed");
    let result = drive(
        None,
        Command::new("dotnet")
            .arg(managed.join("bin/Isoplane.Debugging.Tests/release/Isoplane.Debugging.Tests.dll"))
            .arg("--compiler")
            .arg(
                root.join(
                    "guest-runtime/tests/packages/debugger/hotreload/HotReloadFixture.csproj",
                ),
            )
            .arg(isoplane_fixtures::hot_reload_artifacts().join("debugger.tar"))
            .arg(isoplane_fixtures::hot_reload_artifacts()),
    );
    assert!(
        result.status.success(),
        "compiler baseline startup failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Runs feature scenarios against shared prepared packages with private host/session state.
pub fn prepared_acceptance(mode: &str) {
    let fixture = match mode {
        "watchpoint-steps" => "watchpoints",
        "edit-and-continue"
        | "edit-and-continue-async"
        | "edit-and-continue-try"
        | "edit-and-continue-handler"
        | "edit-and-continue-lines" => "hotreload",
        "hotreload-try"
        | "hotreload-async"
        | "hotreload-unchanged-handler"
        | "hotreload-layout"
        | "hotreload-without-symbols" => "hotreload",
        "statement-awaits-optimized" => "statement-awaits",
        "async-optimized" => "async-stack",
        "async-composed-optimized" => "async-composed",
        "async-valuetask-optimized" => "async-valuetask",
        "async-pooled-optimized" => "async-pooled",
        "async-pooled-composed-optimized" => "async-pooled-composed",
        other => other,
    };
    let root = repository();
    let hot_reload = mode.starts_with("hotreload") || mode.starts_with("edit-and-continue");
    let package = if hot_reload {
        isoplane_fixtures::hot_reload_artifacts().join("debugger.tar")
    } else {
        isoplane_fixtures::mono_package(if mode.ends_with("-optimized") {
            "debugger-optimized"
        } else {
            "debugger"
        })
    };
    let sources = if mode.starts_with("async-") {
        root.join(".build/clr-anycpu/debugger-sources")
    } else {
        root.join("guest-runtime/tests/packages/debugger")
    };
    let source = sources.join(format!("{fixture}.cs"));
    let route = if mode == "edit-and-continue-async" {
        "hotreload-async"
    } else if mode == "edit-and-continue-try" {
        "hotreload-try"
    } else if mode == "edit-and-continue-handler" {
        "hotreload-unchanged-handler"
    } else if mode == "hotreload-without-symbols" {
        "hotreload"
    } else if mode.starts_with("hotreload-") {
        mode
    } else {
        fixture
    };
    let host = root
        .join(".build/clr-anycpu/managed/bin/Isoplane.Cli.Tests/release/Isoplane.Cli.Tests.dll");
    assert!(
        package.is_file() && host.is_file() && source.is_file(),
        "run task test:debugger to prepare and run debugger feature tests"
    );
    let directory = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new("dotnet");
    command
        .arg(host)
        .arg("--debug-fixture")
        .arg(directory.path())
        .arg(&package)
        .arg(isoplane_fixtures::mono_debug_runtime_module());
    if mode.starts_with("edit-and-continue") {
        command
            .arg(sources.join("hotreload/HotReloadFixture.csproj"))
            .arg(isoplane_fixtures::hot_reload_artifacts());
    }
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
        .arg(format!("{}/{route}", dev.url.trim_end_matches('/')))
        .arg(source)
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

#[cfg(unix)]
#[test]
fn dropping_the_debug_driver_closes_descendant_output_pipes() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 60 & printf 'ready\\n'; wait"])
        .process_group(0)
        .stdout(Stdio::piped());
    let mut driver = Driver(
        command
            .spawn()
            .expect("start a driver with an inherited pipe"),
    );
    let stdout = driver.0.stdout.take().expect("capture driver output");
    let (sender, lines) = channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            sender
                .send(line.expect("read driver line"))
                .expect("owner is observing output");
        }
    });
    assert_eq!(
        lines.recv_timeout(Duration::from_secs(10)).unwrap(),
        "ready"
    );
    drop(driver);
    assert!(
        matches!(
            lines.recv_timeout(Duration::from_secs(10)),
            Err(RecvTimeoutError::Disconnected)
        ),
        "driver cleanup must close output inherited by descendants"
    );
    reader.join().expect("driver output reader exited");
}
