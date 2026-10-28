use std::collections::BTreeMap;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use win32job::{ExtendedLimitInfo, Job};
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    JobObjectAssociateCompletionPortInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    executable: String,
    arguments: Vec<String>,
    directory: String,
    environment: BTreeMap<String, String>,
    capture_output: bool,
    capture_errors: bool,
    cleanup_grace_ms: u64,
}

pub(crate) struct OwnedChild {
    child: Mutex<Child>,
    job: Job,
    port: OwnedHandle,
    cleanup_grace: Duration,
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: callers transfer a newly created, valid Win32 handle exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn create_job() -> io::Result<(Job, OwnedHandle)> {
    let mut limits = ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = Job::create_with_limit_info(&limits).map_err(io::Error::other)?;
    // SAFETY: INVALID_HANDLE_VALUE creates a new completion port without attaching a file.
    let port =
        owned(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) })?;
    let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: std::ptr::null_mut(),
        CompletionPort: port.as_raw_handle(),
    };
    // SAFETY: both owned handles remain alive and the information buffer has the specified size.
    if unsafe {
        SetInformationJobObject(
            job.handle() as HANDLE,
            JobObjectAssociateCompletionPortInformation,
            (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
            std::mem::size_of_val(&association) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((job, port))
}

fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: this requests a new read-only snapshot of system threads.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: entry is writable, correctly sized storage; snapshot is an owned thread snapshot.
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: the still-suspended child owns this thread; the child handle prevents PID reuse.
            let thread =
                owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
            // SAFETY: thread is an owned handle with suspend/resume access to the child thread.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: the same correctly sized buffer and owned snapshot remain alive.
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            return Err(
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    io::Error::other("the suspended child has no thread to resume")
                } else {
                    error
                },
            );
        }
    }
}

pub(crate) fn start(settings: &[u8]) -> Result<Vec<u8>, crate::error::Error> {
    let settings: Start = serde_json::from_slice(settings)?;
    let (job, port) = create_job()?;
    let mut child = Command::new(settings.executable)
        .args(settings.arguments)
        .current_dir(settings.directory)
        .env_clear()
        .envs(settings.environment)
        .creation_flags(CREATE_SUSPENDED)
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
    if let Err(error) = job
        .assign_process(child.as_raw_handle() as isize)
        .map_err(io::Error::other)
        .and_then(|()| resume(child.id()))
    {
        child.kill()?;
        child.wait()?;
        return Err(error.into());
    }
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("owned child has no stdin pipe"))?
        .into_raw_handle() as usize;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| pipe.into_raw_handle() as usize);
    let stderr = child
        .stderr
        .take()
        .map(|pipe| pipe.into_raw_handle() as usize);
    let pid = child.id();
    let owner = Box::new(OwnedChild {
        child: Mutex::new(child),
        job,
        port,
        cleanup_grace: Duration::from_millis(settings.cleanup_grace_ms),
    });
    Ok(serde_json::json!({"handle": Box::into_raw(owner) as usize, "pid": pid, "stdin": stdin, "stdout": stdout, "stderr": stderr}).to_string().into_bytes())
}

impl OwnedChild {
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: the owned job is alive, identifies only this process tree, and can be cancelled concurrently with wait.
        if unsafe { TerminateJobObject(self.job.handle() as HANDLE, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait_for_descendants(&self, grace: Duration) -> io::Result<bool> {
        let deadline = Instant::now().checked_add(grace).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "process cleanup grace is too large",
            )
        })?;
        while !self
            .job
            .query_process_id_list()
            .map_err(io::Error::other)?
            .is_empty()
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let mut code = 0;
            let mut key = 0;
            let mut overlapped = std::ptr::null_mut();
            let timeout = remaining.as_millis().clamp(1, (u32::MAX - 1) as u128) as u32;
            // SAFETY: port and all writable output buffers remain alive throughout this blocking call.
            if unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle(),
                    &mut code,
                    &mut key,
                    &mut overlapped,
                    timeout,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(WAIT_TIMEOUT as i32) {
                    return Err(error);
                }
            }
        }
        Ok(true)
    }

    pub fn wait(&self) -> io::Result<i32> {
        let status = self.child.lock().wait()?;
        let finished = self.wait_for_descendants(self.cleanup_grace)?;
        if !finished {
            self.terminate()?;
            if !self.wait_for_descendants(Duration::from_secs(30))? {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned Windows processes did not terminate",
                ));
            }
        }
        Ok(if !finished && status.success() {
            125
        } else {
            status.code().unwrap_or(1)
        })
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self
            .terminate()
            .and_then(|()| self.child.get_mut().wait().map(|_| ()))
        {
            tracing::error!(%error, "owned Windows process cleanup failed");
        }
    }
}
