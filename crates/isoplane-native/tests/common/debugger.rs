//! Coordinate the DAP driver with the development process's shutdown control.

use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::process::{Child, Command, Output, Stdio};

use crate::common::Dev;

/// Runs the DAP driver, relaying shutdown requests to the development owner.
pub fn drive(dev: &mut Dev, command: &mut Command) -> Output {
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
        if line == "request-owner-stop" {
            dev.stop();
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
