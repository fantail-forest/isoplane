use std::collections::BTreeMap;
use std::io;
use std::os::fd::IntoRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};

use parking_lot::Mutex;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    executable: String,
    arguments: Vec<String>,
    directory: String,
    environment: BTreeMap<String, String>,
    capture_output: bool,
    capture_errors: bool,
}

pub(crate) struct OwnedChild {
    child: Mutex<Child>,
    pid: libc::pid_t,
    reaped: Mutex<bool>,
}

pub(crate) fn start(settings: &[u8]) -> Result<Vec<u8>, crate::validation::Error> {
    let settings: Start = serde_json::from_slice(settings)?;
    let mut child = Command::new(settings.executable)
        .args(settings.arguments)
        .current_dir(settings.directory)
        .env_clear()
        .envs(settings.environment)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(if settings.capture_output {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stderr(if settings.capture_errors {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("owned child has no stdin pipe"))?
        .into_raw_fd();
    let stdout = child.stdout.take().map(IntoRawFd::into_raw_fd);
    let stderr = child.stderr.take().map(IntoRawFd::into_raw_fd);
    let pid = child.id() as libc::pid_t;
    let owner = Box::new(OwnedChild {
        child: Mutex::new(child),
        pid,
        reaped: Mutex::new(false),
    });
    Ok(serde_json::json!({ "handle": Box::into_raw(owner) as usize, "pid":pid, "stdin":stdin, "stdout":stdout, "stderr":stderr }).to_string().into_bytes())
}

impl OwnedChild {
    pub fn terminate(&self) -> io::Result<()> {
        let reaped = self.reaped.lock();
        if !*reaped {
            self.kill_group()?;
        }
        Ok(())
    }

    fn kill_group(&self) -> io::Result<()> {
        // SAFETY: the group leader remains unreaped under the reaped lock,
        // preventing its ID from being reused for an unrelated process group.
        if unsafe { libc::kill(-self.pid, libc::SIGKILL) } < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<i32> {
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
            // SAFETY: writable siginfo storage; WNOWAIT retains this owned PID
            // until group cleanup completes and child.wait reaps it below.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            } == 0
            {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        let mut reaped = self.reaped.lock();
        let descendants = self.has_live_members();
        self.kill_group()?;
        let status = self.child.lock().wait()?;
        *reaped = true;
        if descendants? && status.success() {
            return Ok(125);
        }
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
    }

    fn has_live_members(&self) -> io::Result<bool> {
        for process in procfs::process::all_processes().map_err(io::Error::other)? {
            let stat = match process.and_then(|process| process.stat()) {
                Ok(stat) => stat,
                // Processes may exit during enumeration; inaccessible processes
                // belong to another principal, outside this owner's group.
                Err(procfs::ProcError::NotFound(_) | procfs::ProcError::PermissionDenied(_)) => {
                    continue
                }
                Err(error) => return Err(io::Error::other(error)),
            };
            if stat.pid != self.pid && stat.pgrp == self.pid && !matches!(stat.state, 'Z' | 'X') {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !*self.reaped.get_mut() {
            if let Err(error) = self
                .kill_group()
                .and_then(|()| self.child.get_mut().wait().map(|_| ()))
            {
                tracing::error!(pid = self.pid, %error, "owned process cleanup failed");
            }
        }
    }
}
