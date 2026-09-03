//! The OS clock reads that back the per-dispatch CPU meter: `CLOCK_THREAD_CPUTIME_ID` and `CLOCK_MONOTONIC`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Failed clock reads across this process.
static CLOCK_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Exported on the scrape as `clock_read_failures`; non-zero means some recorded
/// interval is a floor rather than a measurement.
pub fn clock_read_failures() -> u64 {
    CLOCK_READ_FAILURES.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Fault injection for the unreadable-clock path; thread-local so one
    /// test's fault is not another's flake.
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

fn read_clock_ns(clock: libc::clockid_t) -> Option<u64> {
    if clock_failure_forced() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, aligned `timespec` owned for the duration of
    // the call; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    if rc != 0 {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some((ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64))
}

/// `CLOCK_THREAD_CPUTIME_ID` for the calling thread, or `None` if the clock
/// could not be read. Per thread, so preemption by a neighbour costs the
/// tenant nothing.
pub fn try_read_thread_cpu_ns() -> Option<u64> {
    read_clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// Raw `CLOCK_MONOTONIC`, with no per-process origin subtracted, or `None` if
/// the clock could not be read.
pub fn try_read_mono_ns() -> Option<u64> {
    read_clock_ns(libc::CLOCK_MONOTONIC)
}

/// [`try_read_thread_cpu_ns`] with the unreadable case flattened to `0`, a
/// floor rather than a measurement; the failure is still counted in
/// [`clock_read_failures`].
pub fn read_thread_cpu_ns() -> u64 {
    try_read_thread_cpu_ns().unwrap_or(0)
}

/// A thread-CPU measurement over one Hot interval, the interval measured.
/// Time between two Hot intervals is inside no `CpuInterval`, which is why a
/// parked instance costs zero CPU.
pub struct CpuInterval {
    /// `None` when the clock could not be read at `start`; a zero start would
    /// attribute the thread's entire lifetime to this interval.
    start: Option<u64>,
}

impl CpuInterval {
    pub fn start() -> Self {
        Self {
            start: try_read_thread_cpu_ns(),
        }
    }
    /// `None` if either end could not be read.
    pub fn try_elapsed_ns(&self) -> Option<u64> {
        Some(try_read_thread_cpu_ns()?.saturating_sub(self.start?))
    }
    /// An unreadable clock flattened to `0`, still counted in
    /// [`clock_read_failures`].
    pub fn elapsed_ns(&self) -> u64 {
        self.try_elapsed_ns().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meters::Meters;

    #[test]
    fn an_unreadable_clock_is_not_a_zero_measurement() {
        let before = clock_read_failures();
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));

        assert_eq!(
            try_read_thread_cpu_ns(),
            None,
            "an unreadable clock must say so"
        );
        let failures = clock_read_failures();
        assert!(failures > before, "the failed read was not counted");

        assert_eq!(read_thread_cpu_ns(), 0, "the lossy wrapper answers a floor");
        assert!(
            clock_read_failures() > failures,
            "and the floor is still on the record as a failure"
        );

        let iv = CpuInterval::start();
        assert_eq!(
            iv.try_elapsed_ns(),
            None,
            "an interval with an unreadable end is not a number"
        );
        let m = Meters::new();
        m.record_interval(iv.try_elapsed_ns());
        assert_eq!(m.cpu_ns(), 0, "an unmeasured interval must not be invented");
        assert_eq!(m.incomplete_intervals(), 1, "...and must not vanish either");

        FORCE_CLOCK_FAILURE.with(|f| f.set(false));

        let iv = CpuInterval::start();
        let mut x = 0u64;
        for i in 0..300_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let measured = iv.try_elapsed_ns();
        assert!(
            measured.is_some(),
            "a working clock must produce a measurement"
        );
        m.record_interval(measured);
        assert!(m.cpu_ns() > 0, "a working clock measures the interval");
        assert_eq!(
            m.incomplete_intervals(),
            1,
            "the working read must not be counted as a fault"
        );
    }

    #[test]
    fn the_clock_fault_injection_does_not_escape_its_thread() {
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));
        assert_eq!(try_read_thread_cpu_ns(), None, "the fault is injected here");
        let elsewhere = std::thread::spawn(try_read_thread_cpu_ns)
            .join()
            .expect("thread");
        assert!(
            elsewhere.is_some(),
            "the injected fault leaked into another thread"
        );
        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
    }

    #[test]
    fn thread_cpu_clock_advances_only_while_burning() {
        let idle = CpuInterval::start();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let slept = idle.elapsed_ns();

        let busy = CpuInterval::start();
        let mut x = 0u64;
        for i in 0..3_000_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let burned = busy.elapsed_ns();

        assert!(slept < 5_000_000, "sleeping recorded {slept} ns of CPU");
        assert!(
            burned > slept,
            "burning {burned} ns did not exceed sleeping {slept} ns"
        );
    }
}
