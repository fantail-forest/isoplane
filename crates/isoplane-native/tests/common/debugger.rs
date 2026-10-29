//! Coordinate the DAP driver with the development process's shutdown control.

use std::io::{Read, Seek, Write};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use isoplane_test_support::{
    assert_success, forward_lines, prepared_path, recv_before, repository_root, ChildGuard,
};

use crate::common::Dev;

/// Runs the DAP driver, relaying shutdown requests to the development owner.
pub fn drive(mut dev: Option<&mut Dev>, command: &mut Command) -> Output {
    let mut errors = tempfile::tempfile().expect("create the debugger diagnostic capture");
    let mut driver = ChildGuard::spawn(
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(
            errors
                .try_clone()
                .expect("share debugger diagnostic capture"),
        ),
        "the DAP driver",
    )
    .expect("start the installed DAP driver");
    let stdout = driver.stdout.take().expect("capture DAP driver output");
    let (sender, lines) = channel();
    forward_lines(stdout, sender, |line| line);
    // Installed workflows include compilation, rejected edits, reload and process teardown.
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut output = Vec::new();
    loop {
        let line = match recv_before(&lines, deadline) {
            Ok(line) => line,
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
                driver.stdin.as_mut().expect("driver control input"),
                "owner-event:{event}"
            )
            .expect("acknowledge the development event");
        }
        if line == "request-owner-stop" {
            dev.as_mut()
                .expect("owner-stop requires an externally started session")
                .stop();
            driver
                .stdin
                .as_mut()
                .expect("the DAP driver must have its control input")
                .write_all(b"owner-stopped\n")
                .expect("acknowledge development shutdown to the DAP driver");
        }
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
    let status = std::process::Child::wait(&mut driver).expect("wait for the DAP driver");
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

/// Loads a prepared compiler baseline without paying for a guest invocation.
pub fn compiler_acceptance() {
    let root = repository_root(env!("CARGO_MANIFEST_DIR"));
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
    assert_success(&result, "compiler baseline startup");
}

/// Runs feature scenarios against shared prepared packages with private host/session state.
pub fn prepared_acceptance(mode: &str) {
    let fixture = match mode {
        "watchpoint-steps" => "watchpoints",
        "edit-and-continue-callers"
        | "edit-and-continue-constructor-callers"
        | "edit-and-continue-running" => "hotreload/Callers",
        "edit-and-continue-iterators" => "hotreload/Iterators",
        "edit-and-continue"
        | "edit-and-continue-async"
        | "edit-and-continue-try"
        | "edit-and-continue-handler"
        | "edit-and-continue-resume"
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
    let root = repository_root(env!("CARGO_MANIFEST_DIR"));
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
    let route = if mode == "edit-and-continue-running" {
        "hotreload-running"
    } else if mode == "edit-and-continue-constructor-callers" {
        "hotreload-constructor-callers"
    } else if mode == "edit-and-continue-callers" {
        "hotreload-callers"
    } else if mode == "edit-and-continue-iterators" {
        "hotreload-iterators"
    } else if mode == "edit-and-continue-async" {
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
        .arg(prepared_path("ISOPLANE_TEST_CLI_ASSEMBLY", "task cli"))
        .arg(metadata["endpoint"].as_str().unwrap())
        .arg(format!("{}/{route}", dev.url.trim_end_matches('/')))
        .arg(source)
        .arg(dev.pid().to_string())
        .arg(mode)
        .arg(&package);
    let result = drive(Some(&mut dev), &mut driver);
    assert_success(
        &result,
        &format!(
            "prepared debug {mode}, with development output:\n{}",
            dev.diagnostics()
        ),
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
