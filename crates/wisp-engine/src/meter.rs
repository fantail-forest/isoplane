//! The compute meter: the calling thread's CPU clock, accumulated over a
//! dispatch's hot intervals. A parked instance holds no thread, so park time
//! is unreachable by the meter rather than excluded by policy.

use std::sync::atomic::{AtomicU64, Ordering};

/// Failed clock reads, process-wide, kept distinct from a measured zero.
static CLOCK_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// How many thread-CPU clock reads have failed in this process. Non-zero means
/// some recorded interval is a floor rather than a measurement.
pub fn clock_read_failures() -> u64 {
    CLOCK_READ_FAILURES.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Thread-local so one test's injected fault is not another test's flake.
    static FORCE_CLOCK_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn clock_failure_forced() -> bool {
    FORCE_CLOCK_FAILURE.with(|f| f.get())
}

#[cfg(not(test))]
#[inline(always)]
fn clock_failure_forced() -> bool {
    false
}

/// Nanoseconds of CPU consumed by the calling thread, or `None` if the clock
/// could not be read.
#[inline]
pub(crate) fn try_thread_cpu_nanos() -> Option<u64> {
    if clock_failure_forced() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let measured = cpu_time::ThreadTime::try_now()
        .ok()
        .and_then(|time| u64::try_from(time.as_duration().as_nanos()).ok());
    if measured.is_none() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    measured
}

/// [`try_thread_cpu_nanos`] with the unreadable case flattened to `0`, for
/// the epoch callback, which has no error channel. On failure the dispatch
/// reads high and is killed at its deadline rather than escaping its budget.
#[inline]
pub(crate) fn thread_cpu_nanos() -> u64 {
    try_thread_cpu_nanos().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test rather than two because the failure counter is process-wide
    /// and test threads run concurrently.
    #[test]
    fn an_unreadable_clock_is_counted_rather_than_read_as_zero() {
        let before = clock_read_failures();
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));

        assert_eq!(
            try_thread_cpu_nanos(),
            None,
            "an unreadable clock must say so rather than answer zero"
        );
        assert_eq!(
            clock_read_failures(),
            before + 1,
            "the failed read must be counted"
        );
        assert_eq!(
            thread_cpu_nanos(),
            0,
            "the flattened form answers the floor"
        );
        assert_eq!(
            clock_read_failures(),
            before + 2,
            "the flattened form must still count the failure"
        );

        let elsewhere = std::thread::spawn(try_thread_cpu_nanos)
            .join()
            .expect("the probe thread does not panic");
        assert!(
            elsewhere.is_some(),
            "the injected fault leaked into another thread"
        );

        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
        assert!(
            try_thread_cpu_nanos().is_some(),
            "a working clock must be readable again"
        );
        assert_eq!(
            clock_read_failures(),
            before + 2,
            "a working read must not be counted as a failure"
        );
    }

    #[test]
    fn the_native_thread_clock_measures_cpu_work() {
        wisp_fixtures::assert_cpu_clock_advances(try_thread_cpu_nanos);
    }
}
