//! A local development process, observed through its output and HTTP listener.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use isoplane_test_support::{
    forward_lines, recv_before, run_successfully, wait_for, ChildGuard, TestChild,
};

pub struct Dev {
    child: ChildGuard<PtyChild>,
    terminal: Option<Terminal>,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
    pub url: String,
    pub debugger: Option<serde_json::Value>,
    output_timeout: Duration,
    observed: String,
}

struct Terminal {
    input: Box<dyn Write + Send>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

/// The development process as the terminal library holds it, whether it
/// runs in a terminal or on pipes; either way it leads its own process group.
pub struct PtyChild(Box<dyn portable_pty::Child + Send>);

impl TestChild for PtyChild {
    fn id(&self) -> u32 {
        self.0
            .process_id()
            .expect("development child has a process identifier")
    }

    fn has_exited(&mut self) -> std::io::Result<bool> {
        self.0.try_wait().map(|status| status.is_some())
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }

    fn wait(&mut self) -> std::io::Result<()> {
        self.0.wait().map(drop)
    }
}

pub fn managed_command(
    project: &std::path::Path,
    executable: &std::path::Path,
    args: &[&str],
) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["dev", "--json", "--port", "0", "--project"])
        .arg(project)
        .args(args);
    if cfg!(windows) {
        command
            .arg("--shutdown-on-stdin-close")
            .stdin(Stdio::piped());
    }
    command
}

impl Dev {
    pub fn start_managed(project: &std::path::Path, executable: &std::path::Path) -> Self {
        Self::start_managed_with_args(project, executable, &[])
    }

    pub fn start_managed_with_args(
        project: &std::path::Path,
        executable: &std::path::Path,
        args: &[&str],
    ) -> Self {
        Self::spawn(
            &mut managed_command(project, executable, args),
            Duration::from_secs(600),
            true,
        )
    }

    /// Starts a session that compiles into `cache` instead of reusing the
    /// runtime Task prepared, for tests of a user's first sessions.
    pub fn start_managed_with_cache(
        project: &std::path::Path,
        executable: &std::path::Path,
        args: &[&str],
        cache: &std::path::Path,
    ) -> Self {
        Self::spawn(
            managed_command(project, executable, args).env("ISOPLANE_CACHE_DIR", cache),
            Duration::from_secs(600),
            true,
        )
    }

    pub fn spawn(command: &mut Command, output_timeout: Duration, structured_stdout: bool) -> Self {
        Self::spawn_inner(command, output_timeout, structured_stdout, true)
    }

    fn spawn_inner(
        command: &mut Command,
        output_timeout: Duration,
        structured_stdout: bool,
        managed_toolchain: bool,
    ) -> Self {
        command.env("ISOPLANE_ENDPOINT", "http://127.0.0.1:1");
        if !command
            .get_envs()
            .any(|(name, _)| name == "ISOPLANE_CACHE_DIR")
        {
            command.env("ISOPLANE_CACHE_DIR", isoplane_fixtures::runtime_cache());
        }
        if managed_toolchain {
            command.env(
                "ISOPLANE_GUEST_RUNTIME_FEED",
                isoplane_fixtures::guest_runtime_feed(),
            );
        }
        let (child, terminal, streams, input) = if structured_stdout {
            #[cfg(unix)]
            command.process_group(0);
            let mut child = command
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let input = child.stdin.take();
            (
                Box::new(child) as Box<dyn portable_pty::Child + Send>,
                None,
                vec![
                    (Box::new(stdout) as Box<dyn std::io::Read + Send>, true),
                    (Box::new(stderr) as Box<dyn std::io::Read + Send>, false),
                ],
                input,
            )
        } else {
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    rows: 40,
                    cols: 200,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("create a real development terminal");
            let mut invocation = portable_pty::CommandBuilder::new(command.get_program());
            invocation.args(command.get_args());
            if let Some(directory) = command.get_current_dir() {
                invocation.cwd(directory);
            }
            for (name, value) in command.get_envs() {
                match value {
                    Some(value) => invocation.env(name, value),
                    None => invocation.env_remove(name),
                }
            }
            invocation.env("NO_COLOR", "1");
            invocation.env("TERM", "dumb");
            let child = pair
                .slave
                .spawn_command(invocation)
                .expect("start development tooling in its terminal");
            drop(pair.slave);
            let reader = pair.master.try_clone_reader().unwrap();
            let mut input = pair.master.take_writer().unwrap();
            if cfg!(windows) {
                // ConPTY inherits the cursor and waits for this terminal's initial position.
                input.write_all(b"\x1b[1;1R").unwrap();
                input.flush().unwrap();
            }
            (
                child as Box<dyn portable_pty::Child + Send>,
                Some(Terminal {
                    input,
                    _master: pair.master,
                }),
                vec![(reader, false)],
                None,
            )
        };
        let (tx, lines) = channel();
        for (stream, machine) in streams {
            forward_lines(stream, tx.clone(), move |line| {
                let line = if structured_stdout {
                    line
                } else {
                    strip_ansi_escapes::strip_str(line)
                };
                if !structured_stdout {
                    eprintln!("{line}");
                }
                if machine && serde_json::from_str::<serde_json::Value>(&line).is_err() {
                    format!("invalid machine output: {line}")
                } else {
                    line
                }
            });
        }
        drop(tx);
        let mut dev = Self {
            child: ChildGuard::new(PtyChild(child), "the development process"),
            terminal,
            input,
            lines,
            url: String::new(),
            debugger: None,
            output_timeout,
            observed: String::new(),
        };
        let ready = dev.wait_matching("ready", |line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|record| record["event"] == "ready")
                || line.contains(" ready ") && line.contains(" at http://")
        });
        dev.url = serde_json::from_str::<serde_json::Value>(&ready)
            .ok()
            .and_then(|record| record["url"].as_str().map(str::to_string))
            .or_else(|| {
                ready
                    .split_once(" at ")
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .map(str::to_string)
            })
            .expect("development ready output contains its local URL");
        dev
    }

    pub fn wait_event(&mut self, event: &str) -> serde_json::Value {
        let line = self.wait_matching(event, |line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|record| record["event"] == event)
        });
        serde_json::from_str(&line).unwrap()
    }

    pub fn wait_for(&mut self, marker: &str) -> String {
        self.wait_matching(marker, |line| line.contains(marker))
    }

    fn wait_matching(&mut self, marker: &str, matches: impl Fn(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + self.output_timeout;
        let mut output = String::new();
        loop {
            let line = recv_before(&self.lines, deadline).unwrap_or_else(|e| {
                panic!(
                    "local dev never reported {marker:?}: {e}; process status: {:?}\n{output}",
                    self.child.0.try_wait()
                )
            });
            assert!(
                !line.starts_with("invalid machine output:"),
                "dev --json must reserve stdout for structured records: {line}"
            );
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) {
                if record["event"] == "debugger_ready" {
                    self.debugger = Some(record);
                }
            }
            self.observed.push_str(&line);
            self.observed.push('\n');
            output.push_str(&line);
            output.push('\n');
            if matches(&line) {
                return line;
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(terminal) = &mut self.terminal {
            terminal
                .input
                .write_all(b"\x03")
                .expect("send Ctrl+C through the development terminal");
            terminal.input.flush().unwrap();
        } else if let Some(input) = self.input.take() {
            drop(input);
        } else {
            run_successfully(
                Command::new("kill").args(["-INT", "--", &format!("-{}", self.pid())]),
                "send the local process its shutdown signal",
            );
        }
        self.wait_exit();
    }

    pub fn diagnostics(&self) -> String {
        format!(
            "{}{}",
            self.observed,
            self.lines.try_iter().collect::<Vec<_>>().join("\n")
        )
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn wait_exit(&mut self) {
        let mut status = None;
        assert!(
            wait_for(
                || {
                    status = self.child.0.try_wait().unwrap();
                    status.is_some()
                },
                Duration::from_secs(120)
            ),
            "local dev hung during shutdown"
        );
        let status = status.expect("the wait ended with a status");
        assert!(
            status.success(),
            "local dev must shut down cleanly: {status}\n{}",
            self.diagnostics()
        );
    }
}

pub fn guest(body: &str) -> Vec<u8> {
    guest_module(body, None)
}

pub fn guest_with_log(body: &str, message: &str) -> Vec<u8> {
    guest_module(body, Some(message))
}

fn guest_module(body: &str, message: Option<&str>) -> Vec<u8> {
    let response = isoplane_abi::dispatch::ResponseHead {
        status: 200,
        headers: vec![],
        body: body.as_bytes().to_vec(),
        streaming: false,
    };
    let bytes = isoplane_abi::encode(&response).unwrap();
    let escaped = isoplane_test_support::wat_bytes(&bytes);
    let log = message.map_or_else(String::new, |message| {
        let bytes = message.as_bytes();
        let escaped = isoplane_test_support::wat_bytes(bytes);
        format!(
            "(import \"isoplane\" \"log_emit\" (func $log (param i32 i32 i32)))\n\
             (data (i32.const 8192) \"{escaped}\")"
        )
    });
    let emit = message.map_or_else(String::new, |message| {
        format!(
            "(call $log (i32.const 2) (i32.const 8192) (i32.const {}))",
            message.len()
        )
    });
    wat::parse_str(format!(r#"(module
        (import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
        {log}
        (memory (export "memory") 2 2)
        (data (i32.const 4096) "{escaped}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 16384))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            {emit}
            (i64.shl (i64.extend_i32_u (call $response (i32.const 4096) (i32.const {}))) (i64.const 32)))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#, bytes.len())).unwrap()
}

#[cfg(unix)]
#[test]
fn dropping_the_harness_closes_descendant_output_pipes() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        r#"sleep 60 & printf '{"event":"ready","url":"http://127.0.0.1:1"}\n'; wait"#,
    ]);
    let mut dev = Dev::spawn(&mut command, Duration::from_secs(10), true);
    let (_, unused) = channel();
    let lines = std::mem::replace(&mut dev.lines, unused);
    drop(dev);
    assert!(
        matches!(
            lines.recv_timeout(Duration::from_secs(10)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ),
        "dropping the CLI harness must terminate descendants that keep its output pipes open"
    );
}
