#[path = "input.rs"]
mod input;

use std::ffi::OsString;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

enum Event {
    InputClosed,
    ChildExited,
    Failed(io::Error),
}

pub(super) fn run(command: Vec<OsString>, grace: Duration) -> io::Result<i32> {
    // SAFETY: these calls modify only this single-threaded supervisor's session
    // and orphan adoption; there are no guest code or foreign pointers involved.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: PR_SET_CHILD_SUBREAPER consumes an integer flag, not a pointer.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .process_group(0)
        .stdin(Stdio::piped())
        .spawn()?;
    let owner = Owner(child.id() as libc::pid_t);
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("child input was not piped"))?;
    let (sender, receiver) = mpsc::channel();
    let input_sender = sender.clone();
    std::thread::spawn(move || {
        let result = input::forward_to(input);
        let event = match result {
            Ok(()) => Event::InputClosed,
            Err(error) => Event::Failed(error),
        };
        if input_sender.send(event).is_err() {
            eprintln!("isoplane: input closed after process owner stopped");
        }
    });
    let pid = owner.0;
    std::thread::spawn(move || {
        let result = wait_without_reaping(pid);
        let event = match result {
            Ok(()) => Event::ChildExited,
            Err(error) => Event::Failed(error),
        };
        if sender.send(event).is_err() {
            eprintln!("isoplane: child exited after process owner stopped");
        }
    });
    let event = receiver.recv().map_err(io::Error::other)?;
    let event = if matches!(event, Event::InputClosed) && !grace.is_zero() {
        match receiver.recv_timeout(grace) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => Event::InputClosed,
            Err(error) => return Err(io::Error::other(error)),
        }
    } else {
        event
    };
    let orphans = matches!(event, Event::ChildExited) && wait_for_orphans(owner.0, grace)?;
    owner.terminate()?;
    let status = child.wait()?;
    std::mem::forget(owner);
    reap_orphans()?;
    if let Event::Failed(error) = event {
        return Err(error);
    }
    if orphans && status.success() {
        eprintln!("isoplane: the command exited while owned descendants remained; terminated those descendants");
        return Ok(125);
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

fn wait_without_reaping(pid: libc::pid_t) -> io::Result<()> {
    loop {
        let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
        // SAFETY: status points to valid writable siginfo_t storage. WNOWAIT
        // retains the child PID until group termination, preventing PID reuse.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                status.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct Owner(libc::pid_t);

impl Owner {
    fn terminate(&self) -> io::Result<()> {
        // SAFETY: the unreaped child owns this group ID. A negative ID selects
        // that group only, never the caller's or another invocation's group.
        if unsafe { libc::kill(-self.0, libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            eprintln!(
                "isoplane: could not terminate owned process group {}: {error}",
                self.0
            );
        }
    }
}

fn reap_orphans() -> io::Result<()> {
    loop {
        let children =
            std::fs::read_to_string(format!("/proc/self/task/{}/children", std::process::id()))?;
        for child in children.split_whitespace() {
            let pid: libc::pid_t = child.parse().map_err(io::Error::other)?;
            // SAFETY: only this supervisor's direct, unreaped children appear
            // here. No other thread reaps them, so their PIDs cannot be reused.
            if unsafe { libc::kill(pid, libc::SIGKILL) } < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        // SAFETY: no status is requested. This process is a dedicated subreaper;
        // its application child has already been waited, and all others are owned.
        if unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(());
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn live_orphan(application: libc::pid_t) -> io::Result<Option<libc::pid_t>> {
    let children =
        std::fs::read_to_string(format!("/proc/self/task/{}/children", std::process::id()))?;
    for child in children.split_whitespace() {
        let pid: libc::pid_t = child.parse().map_err(io::Error::other)?;
        if pid == application {
            continue;
        }
        let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: valid writable status storage; WNOWAIT keeps this owned
        // child unreaped while distinguishing live work from finished work.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                status.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: waitid succeeded and initialized status (zero PID means
        // no exit event); si_pid is valid for waitid's child status record.
        if unsafe { status.assume_init().si_pid() } == 0 {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

fn wait_for_orphans(application: libc::pid_t, grace: Duration) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + grace;
    while let Some(pid) = live_orphan(application)? {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(true);
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            if sender.send(wait_without_reaping(pid)).is_err() {
                eprintln!("isoplane: descendant exited after its cleanup deadline");
            }
        });
        match receiver.recv_timeout(remaining) {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(true),
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    Ok(false)
}
