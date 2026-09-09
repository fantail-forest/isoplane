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
                tracing::warn!("thread CPU clock unreadable; reporting init_cpu_ms = 0");
                0
            }
        }
    }
}

fn thread_cpu_ns() -> Option<u64> {
    cpu_time::ThreadTime::try_now()
        .ok()
        .and_then(|time| u64::try_from(time.as_duration().as_nanos()).ok())
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

    #[test]
    fn the_native_thread_clock_measures_cpu_work() {
        wisp_fixtures::assert_cpu_clock_advances(thread_cpu_ns);
    }
}
