//! CPU time of the calling thread, for the deploy record. The thread clock
//! rather than wall time or the process clock because `init_cpu_ms` reads as
//! a property of the tenant's module: wall time moves with whoever else is
//! deploying, and the process clock charges a deploy for every other thread
//! in the daemon.

pub(super) struct ThreadCpu {
    start: Option<u64>,
}

impl ThreadCpu {
    pub(super) fn start() -> Self {
        Self {
            start: thread_cpu_ns(),
        }
    }

    /// Milliseconds since construction; zero, with a warning, when the clock
    /// cannot be read.
    pub(super) fn elapsed_ms(&self) -> u64 {
        match (self.start, thread_cpu_ns()) {
            (Some(started), Some(now)) => now.saturating_sub(started) / 1_000_000,
            _ => {
                tracing::warn!("CLOCK_THREAD_CPUTIME_ID unreadable; reporting init_cpu_ms = 0");
                0
            }
        }
    }
}

fn thread_cpu_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, aligned `timespec` owned for the duration of the
    // call; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (rc == 0).then(|| (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_clock_reports_zero_not_the_threads_lifetime() {
        assert_eq!(
            ThreadCpu { start: None }.elapsed_ms(),
            0,
            "a deploy whose clock could not be read must not appear to have cost minutes"
        );
    }
}
