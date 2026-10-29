//! Child processes under test: the test binary re-run as a child, commands
//! that must succeed, children whose process tree ends with the test, and
//! their output read within a deadline.

use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader, Read};
use std::ops::{Deref, DerefMut};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Instant;

/// A test that re-runs its own binary as a child process, which it tells
/// apart from the parent by a marker variable in the child's environment.
///
/// `name` is the test's path as the harness filters it: the bare function
/// name in an integration test, `tests::name` for a unit test in `mod tests`.
/// A name the binary does not hold runs no test, so the child succeeds
/// without doing anything.
pub struct ChildTest {
    name: &'static str,
}

impl ChildTest {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }

    fn marker(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("ISOPLANE_TEST_CHILD_{name}")
    }

    /// `Some` in the child, holding what the parent gave [`ChildTest::command`].
    pub fn child_value(&self) -> Option<OsString> {
        std::env::var_os(self.marker())
    }

    /// The test binary, about to run this test alone with `value` under the
    /// marker, with output uncaptured so the parent can read it.
    pub fn command(&self, value: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("the test binary's path"));
        command
            .args(["--exact", self.name, "--nocapture"])
            .env(self.marker(), value);
        command
    }
}

/// Fails the test unless `output` reports success, showing both streams.
/// `what` says what the command was doing.
pub fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what}: the command failed with {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs `command` to completion and returns its output, failing the test
/// when it cannot start or does not succeed. `what` says what the command
/// is doing.
pub fn run_successfully(command: &mut Command, what: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{what}: could not run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{what}: {command:?} failed with {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// What [`ChildGuard`] needs of a child process. `std::process::Child`
/// provides it; a harness that holds a child through another library
/// implements it on a wrapper.
pub trait TestChild {
    /// The child's process id.
    fn id(&self) -> u32;
    /// Whether the child has exited, without waiting for it.
    fn has_exited(&mut self) -> io::Result<bool>;
    /// Ends the child without waiting for it.
    fn kill(&mut self) -> io::Result<()>;
    /// Waits for the child to exit.
    fn wait(&mut self) -> io::Result<()>;
}

impl TestChild for Child {
    fn id(&self) -> u32 {
        Child::id(self)
    }

    fn has_exited(&mut self) -> io::Result<bool> {
        self.try_wait().map(|status| status.is_some())
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<()> {
        Child::wait(self).map(drop)
    }
}

/// A child running in its own process group, whose whole tree is killed and
/// which is reaped when the guard drops, so a failed test leaves no
/// descendant holding its output pipes. `what` names the child in
/// diagnostics. The guard derefs to the child for everything else.
pub struct ChildGuard<C: TestChild = Child> {
    child: C,
    what: &'static str,
}

impl ChildGuard<Child> {
    /// Spawns `command` as the leader of a new process group.
    pub fn spawn(command: &mut Command, what: &'static str) -> io::Result<Self> {
        #[cfg(unix)]
        command.process_group(0);
        command.spawn().map(|child| Self::new(child, what))
    }
}

impl<C: TestChild> ChildGuard<C> {
    /// Guards `child`, which runs in its own process group.
    pub fn new(child: C, what: &'static str) -> Self {
        Self { child, what }
    }
}

impl<C: TestChild> Deref for ChildGuard<C> {
    type Target = C;

    fn deref(&self) -> &C {
        &self.child
    }
}

impl<C: TestChild> DerefMut for ChildGuard<C> {
    fn deref_mut(&mut self) -> &mut C {
        &mut self.child
    }
}

impl<C: TestChild> Drop for ChildGuard<C> {
    fn drop(&mut self) {
        let exited = match self.child.has_exited() {
            Ok(exited) => exited,
            Err(error) => {
                eprintln!("could not inspect {}: {error}", self.what);
                false
            }
        };
        terminate_process_tree(self.child.id(), exited);
        if exited {
            return;
        }
        if let Err(error) = self.child.kill() {
            eprintln!("could not stop {}: {error}", self.what);
        }
        if let Err(error) = self.child.wait() {
            eprintln!("could not reap {}: {error}", self.what);
        }
    }
}

/// Stops the process group `pid` leads, including descendants that retain
/// its output pipes. An exited leader may have no group left to signal.
fn terminate_process_tree(pid: u32, exited: bool) {
    #[cfg(windows)]
    if exited {
        return;
    }
    #[cfg(windows)]
    let result = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
    #[cfg(unix)]
    let result = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .output();
    match result {
        Ok(output) if output.status.success() || exited => {}
        Ok(output) => eprintln!(
            "could not stop test process tree {pid}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => eprintln!("could not signal test process tree {pid}: {error}"),
    }
}

/// Queues each line of `stream`, as `render` shapes it, on `lines` from a
/// thread of its own, until the stream ends or nothing receives any more.
/// The thread fails on a read error, which disconnects `lines`.
pub fn forward_lines<T: Send + 'static>(
    stream: impl Read + Send + 'static,
    lines: Sender<T>,
    mut render: impl FnMut(String) -> T + Send + 'static,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let line = line.unwrap_or_else(|error| panic!("reading child output: {error}"));
            if lines.send(render(line)).is_err() {
                break;
            }
        }
    });
}

/// The next value from `receiver`, waiting until `deadline` rather than for
/// an interval, so a loop of these waits for the deadline as a whole.
pub fn recv_before<T>(receiver: &Receiver<T>, deadline: Instant) -> Result<T, RecvTimeoutError> {
    receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
}

#[cfg(test)]
mod tests {
    use super::{
        assert_success, forward_lines, recv_before, run_successfully, ChildGuard, ChildTest,
    };
    use std::process::Command;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::{Duration, Instant};

    #[test]
    fn a_child_test_sees_the_value_its_parent_passed() {
        const TEST: ChildTest =
            ChildTest::new("process::tests::a_child_test_sees_the_value_its_parent_passed");
        if let Some(value) = TEST.child_value() {
            println!("the child saw {}", value.to_string_lossy());
            return;
        }
        let output = run_successfully(&mut TEST.command("marker-7"), "re-run the test as a child");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("the child saw marker-7"),
            "the child must run the test with the parent's value:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_fails_the_test_with_both_streams() {
        let failed = std::panic::catch_unwind(|| {
            run_successfully(
                Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
                "a command that fails",
            )
        });
        let message = failed
            .expect_err("a failing command must fail the test")
            .downcast::<String>()
            .expect("the failure is a formatted message");
        assert!(
            message.starts_with("a command that fails:")
                && message.contains("out\n")
                && message.contains("err\n"),
            "the failure names the operation and shows both streams: {message}"
        );
        let output = Command::new("sh")
            .args(["-c", "exit 1"])
            .output()
            .expect("run sh");
        assert!(
            std::panic::catch_unwind(|| assert_success(&output, "an exit status")).is_err(),
            "an unsuccessful status must fail the assertion"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_guard_closes_pipes_its_descendants_inherited() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 60 & printf 'ready\\n'; wait"])
            .stdout(std::process::Stdio::piped());
        let mut child = ChildGuard::spawn(&mut command, "a shell with a background child")
            .expect("start the shell");
        let (sender, lines) = channel();
        forward_lines(child.stdout.take().expect("piped stdout"), sender, |line| {
            line
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(
            recv_before(&lines, deadline).expect("the shell announces itself"),
            "ready"
        );
        drop(child);
        assert!(
            matches!(
                recv_before(&lines, Instant::now() + Duration::from_secs(10)),
                Err(RecvTimeoutError::Disconnected)
            ),
            "dropping the guard must end the descendant that inherited the pipe"
        );
    }

    #[test]
    fn forwarded_lines_arrive_rendered_and_end_with_the_stream() {
        let (sender, lines) = channel();
        forward_lines(
            std::io::Cursor::new(b"one\ntwo\n".to_vec()),
            sender,
            |line| line.to_uppercase(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(recv_before(&lines, deadline).expect("first line"), "ONE");
        assert_eq!(recv_before(&lines, deadline).expect("second line"), "TWO");
        assert!(
            matches!(
                recv_before(&lines, deadline),
                Err(RecvTimeoutError::Disconnected)
            ),
            "the stream's end disconnects the receiver"
        );
        let (_held, empty) = channel::<String>();
        assert!(
            matches!(
                recv_before(&empty, Instant::now() - Duration::from_secs(1)),
                Err(RecvTimeoutError::Timeout)
            ),
            "a passed deadline times out at once"
        );
    }
}
