use std::io;

use libproc::processes::{pids_by_type, ProcFilter};
use rustix::event::kqueue::{
    kevent_timespec, kqueue, Event, EventFilter, EventFlags, ProcessEvents,
};
use rustix::event::Timespec;
use rustix::fd::OwnedFd;
use rustix::process::{getpgid, Pid};

pub(super) fn group_is_dead(group: libc::pid_t) -> io::Result<bool> {
    let leader =
        Pid::from_raw(group).ok_or_else(|| io::Error::other("owned process has an invalid PID"))?;
    Ok(watch_exit(leader)?.is_none() && live_members(group)?.is_empty())
}

pub(super) fn live_members(group: libc::pid_t) -> io::Result<Vec<OwnedFd>> {
    let mut members = Vec::new();
    for raw in pids_by_type(ProcFilter::ByProgramGroup {
        pgrpid: group as u32,
    })? {
        if raw == group as u32 {
            continue;
        }
        let pid = Pid::from_raw(raw as libc::pid_t)
            .ok_or_else(|| io::Error::other("owned process has an invalid PID"))?;
        if let Some(watcher) = watch_exit(pid)? {
            match getpgid(Some(pid)) {
                Ok(current) if current.as_raw_pid() == group => members.push(watcher),
                // A member can exit or leave the group while the watch is registered.
                Ok(_) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(members)
}

fn watch_exit(pid: Pid) -> io::Result<Option<OwnedFd>> {
    let watcher = kqueue()?;
    let change = Event::new(
        EventFilter::Proc {
            pid,
            flags: ProcessEvents::EXIT,
        },
        EventFlags::ADD | EventFlags::ONESHOT,
        std::ptr::null_mut(),
    );
    let mut events = [change];
    // SAFETY: this registration watches a PID, not a borrowed descriptor or
    // user-data pointer; the returned OwnedFd keeps the queue alive for polling.
    let result = unsafe {
        kevent_timespec(
            &watcher,
            &[change],
            &mut events[..],
            Some(&Timespec::default()),
        )
    };
    match result {
        Ok(0) => Ok(Some(watcher)),
        Ok(_) if events[0].flags().contains(EventFlags::ERROR) => {
            let error = io::Error::from_raw_os_error(events[0].data() as i32);
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Ok(_) | Err(rustix::io::Errno::SRCH) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
