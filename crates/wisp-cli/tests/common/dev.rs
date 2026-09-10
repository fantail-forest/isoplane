//! A local development process, observed through its output and HTTP listener.

use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct Dev {
    child: Child,
    lines: Receiver<String>,
    pub url: String,
    output_timeout: Duration,
}

impl Dev {
    pub fn start(project: &std::path::Path, args: &[&str]) -> Self {
        Self::start_with_timeout(project, args, Duration::from_secs(120))
    }

    pub fn start_with_timeout(
        project: &std::path::Path,
        args: &[&str],
        output_timeout: Duration,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wisp"));
        command
            .args(["--no-color", "--json", "dev", "--port", "0", "--watch"])
            .arg(project)
            .args(args);
        Self::spawn(&mut command, output_timeout)
    }

    pub fn start_managed(project: &std::path::Path, executable: &std::path::Path) -> Self {
        Self::start_managed_with_args(project, executable, &[])
    }

    pub fn start_managed_with_args(
        project: &std::path::Path,
        executable: &std::path::Path,
        args: &[&str],
    ) -> Self {
        let mut command = Command::new(executable);
        command
            .args(["dev", "--json", "--port", "0", "--project"])
            .arg(project)
            .args(args);
        Self::spawn(&mut command, Duration::from_secs(600))
    }

    fn spawn(command: &mut Command, output_timeout: Duration) -> Self {
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .env("WISP_CLI", env!("CARGO_BIN_EXE_wisp"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = channel();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        for (stream, machine) in [
            (Box::new(stdout) as Box<dyn std::io::Read + Send>, true),
            (Box::new(stderr) as Box<dyn std::io::Read + Send>, false),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let line = line.expect("development output remains readable");
                    let line =
                        if machine && serde_json::from_str::<serde_json::Value>(&line).is_err() {
                            format!("invalid machine output: {line}")
                        } else {
                            line
                        };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut dev = Self {
            child,
            lines,
            url: String::new(),
            output_timeout,
        };
        let ready = dev.wait_event("ready");
        dev.url = ready["url"].as_str().unwrap().into();
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
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|e| panic!("local dev never reported {marker:?}: {e}\n{output}"));
            assert!(
                !line.starts_with("invalid machine output:"),
                "dev --json must reserve stdout for structured records: {line}"
            );
            output.push_str(&line);
            output.push('\n');
            if matches(&line) {
                return line;
            }
        }
    }

    pub fn stop(&mut self) {
        let status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "the local process must receive its shutdown signal"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "local dev hung during shutdown"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "local dev must shut down cleanly: {status}"
        );
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let group = format!("-{}", self.child.id());
            match Command::new("kill").args(["-KILL", "--", &group]).output() {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    // A gracefully stopped CLI may already have removed its whole group.
                    if !matches!(self.child.try_wait(), Ok(Some(_))) {
                        eprintln!(
                            "could not stop development process group {group}: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                }
                Err(error) => {
                    eprintln!("could not signal development process group {group}: {error}")
                }
            }
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    eprintln!("could not stop the development process: {error}");
                }
                if let Err(error) = self.child.wait() {
                    eprintln!("could not reap the development process: {error}");
                }
            }
            Err(e) => eprintln!("could not inspect development process: {e}"),
        }
    }
}

pub fn manifest(dir: &std::path::Path, extra: &str) {
    std::fs::write(
        dir.join("wisp.toml"),
        format!("name='local-app'\ntenant='test-owner'\nmodule='app.wasm'\n{extra}"),
    )
    .unwrap();
}

pub fn guest(body: &str) -> Vec<u8> {
    let response = wisp_abi::dispatch::ResponseHead {
        status: 200,
        headers: vec![],
        body: body.as_bytes().to_vec(),
        streaming: false,
    };
    let bytes = wisp_abi::encode(&response).unwrap();
    let escaped: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
    wat::parse_str(format!(r#"(module
        (import "wisp" "resp_new" (func $response (param i32 i32) (result i32)))
        (memory (export "memory") 2 2)
        (data (i32.const 4096) "{escaped}")
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 16384))
        (func (export "wisp_handle") (param i32 i32) (result i64)
            (i64.shl (i64.extend_i32_u (call $response (i32.const 4096) (i32.const {}))) (i64.const 32)))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#, bytes.len())).unwrap()
}

#[cfg(unix)]
#[test]
fn dropping_the_harness_closes_descendant_output_pipes() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        r#"sleep 60 & printf '{"event":"ready","url":"http://127.0.0.1:1"}\n'; wait"#,
    ]);
    let mut dev = Dev::spawn(&mut command, Duration::from_secs(10));
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
