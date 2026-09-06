//! A local development process, observed through its output and HTTP listener.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct Dev {
    child: Child,
    lines: Receiver<String>,
    pub url: String,
}

impl Dev {
    pub fn start(project: &std::path::Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wisp"))
            .args(["--no-color", "dev", "--port", "0", "--watch"])
            .arg(project)
            .args(args)
            .env("WISP_CLI", env!("CARGO_BIN_EXE_wisp"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = channel();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        for stream in [
            Box::new(stdout) as Box<dyn std::io::Read + Send>,
            Box::new(stderr),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let line = line.expect("development output remains readable");
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
        };
        let ready = dev.wait_for("ready ");
        dev.url = ready
            .split(" at ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .into();
        dev
    }

    pub fn wait_for(&mut self, marker: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let mut output = String::new();
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or_else(|e| panic!("local dev never reported {marker:?}: {e}\n{output}"));
            output.push_str(&line);
            output.push('\n');
            if line.contains(marker) {
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
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.child.kill().expect("stop the development process");
                self.child.wait().expect("reap the development process");
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
