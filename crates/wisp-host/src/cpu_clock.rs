//! Native thread CPU and monotonic clocks used by dispatch accounting.

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

fn read_clock_ns(read: impl FnOnce() -> Option<u64>) -> Option<u64> {
    let measured = if clock_failure_forced() { None } else { read() };
    if measured.is_none() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    measured
}

/// CPU consumed by the calling thread, or `None` if its clock cannot be read.
/// Preemption by a neighbour does not consume this thread's CPU budget.
pub fn try_read_thread_cpu_ns() -> Option<u64> {
    read_clock_ns(|| {
        cpu_time::ThreadTime::try_now()
            .ok()
            .and_then(|time| u64::try_from(time.as_duration().as_nanos()).ok())
    })
}

/// Native monotonic nanoseconds without a per-process origin subtraction.
/// Returns `None` if the clock cannot be read or its value cannot be represented.
pub fn try_read_mono_ns() -> Option<u64> {
    read_clock_ns(monotonic_ns)
}

#[cfg(unix)]
fn monotonic_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is writable and correctly aligned, and CLOCK_MONOTONIC selects
    // the supported system monotonic clock rather than a process-owned handle.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return None;
    }
    u64::try_from(ts.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(ts.tv_nsec).ok()?)
}

#[cfg(windows)]
fn monotonic_ns() -> Option<u64> {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let mut ticks = 0;
    let mut frequency = 0;
    // SAFETY: both outputs are live, aligned i64 values; neither operation
    // retains its pointer or requires a caller-owned resource handle.
    let read = unsafe {
        QueryPerformanceCounter(&mut ticks) != 0 && QueryPerformanceFrequency(&mut frequency) != 0
    };
    read.then_some(())?;
    ticks_to_ns(ticks, frequency)
}

#[cfg(any(windows, test))]
fn ticks_to_ns(ticks: i64, frequency: i64) -> Option<u64> {
    let ticks = u128::try_from(ticks).ok()?;
    let frequency = u128::try_from(frequency).ok()?;
    u64::try_from(ticks.checked_mul(1_000_000_000)?.checked_div(frequency)?).ok()
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
    fn a_monotonic_clock_failure_remains_missing_and_counted() {
        let before = clock_read_failures();
        FORCE_CLOCK_FAILURE.with(|failure| failure.set(true));
        let measured = try_read_mono_ns();
        FORCE_CLOCK_FAILURE.with(|failure| failure.set(false));
        assert_eq!(
            measured, None,
            "a failed monotonic read must not become a timestamp"
        );
        assert!(
            clock_read_failures() > before,
            "the monotonic clock failure must be recorded"
        );
        assert!(
            try_read_mono_ns().is_some(),
            "the native monotonic clock must recover after injection"
        );
    }

    #[test]
    fn native_counter_ticks_preserve_subsecond_precision() {
        assert_eq!(
            ticks_to_ns(12_345_678, 10_000_000),
            Some(1_234_567_800),
            "counter conversion must retain fractional seconds"
        );
    }

    #[test]
    fn an_unrepresentable_native_counter_is_not_a_timestamp() {
        for (ticks, frequency) in [(-1, 1), (1, -1), (1, 0), (i64::MAX, 1)] {
            assert_eq!(
                ticks_to_ns(ticks, frequency),
                None,
                "invalid or overflowing native clock values must remain unreadable"
            );
        }
    }

    #[test]
    fn the_native_thread_clock_measures_cpu_work() {
        wisp_fixtures::assert_cpu_clock_advances(try_read_thread_cpu_ns);
    }
}
