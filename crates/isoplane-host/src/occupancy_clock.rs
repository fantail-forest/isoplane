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
    if clock_failure_forced() {
        None
    } else {
        read()
    }
}

/// Monotonic timestamp for measuring occupied worker intervals.
/// Time descheduled by the OS still occupies the worker and consumes its budget.
pub fn try_read_occupancy_ns() -> Option<u64> {
    #[cfg(test)]
    if let Some(value) = TEST_READINGS.with(|readings| {
        readings
            .borrow_mut()
            .as_mut()
            .map(|values| values.pop_front().expect("scripted clock exhausted"))
    }) {
        return Some(value);
    }
    read_clock_ns(isoplane_engine::try_occupancy_nanos)
}

#[cfg(test)]
thread_local! {
    static TEST_READINGS: std::cell::RefCell<Option<std::collections::VecDeque<u64>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_readings<T>(
    values: impl IntoIterator<Item = u64>,
    test: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_READINGS.with(|readings| *readings.borrow_mut() = None);
        }
    }
    TEST_READINGS.with(|readings| {
        assert!(readings.borrow().is_none(), "clock scripts cannot nest");
        *readings.borrow_mut() = Some(values.into_iter().collect());
    });
    let _reset = Reset;
    let result = test();
    TEST_READINGS.with(|readings| {
        assert!(
            readings
                .borrow()
                .as_ref()
                .expect("script installed")
                .is_empty(),
            "scripted clock readings were not consumed"
        )
    });
    result
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

/// A worker-occupancy measurement over one slice or reactor poll, the only
/// interval measured. Time between them is inside no `OccupancyInterval`, which is why a
/// parked instance costs zero occupancy.
pub struct OccupancyInterval {
    /// `None` when the clock could not be read at `start`; a zero start would
    /// attribute time before this interval to this interval.
    start: Option<u64>,
}

impl OccupancyInterval {
    pub fn start() -> Self {
        Self {
            start: try_read_occupancy_ns(),
        }
    }
    /// `None` if either end could not be read.
    pub fn try_elapsed_ns(&self) -> Option<u64> {
        Some(try_read_occupancy_ns()?.saturating_sub(self.start?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meters::Meters;

    #[test]
    fn an_unreadable_clock_is_not_a_zero_measurement() {
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));

        assert_eq!(
            try_read_occupancy_ns(),
            None,
            "an unreadable clock must say so"
        );

        let iv = OccupancyInterval::start();
        assert_eq!(
            iv.try_elapsed_ns(),
            None,
            "an interval with an unreadable end is not a number"
        );
        let m = Meters::new();
        m.record_interval(iv.try_elapsed_ns());
        assert_eq!(
            m.occupancy_ns(),
            0,
            "an unmeasured interval must not be invented"
        );

        FORCE_CLOCK_FAILURE.with(|f| f.set(false));

        let iv = OccupancyInterval::start();
        let measured = iv.try_elapsed_ns();
        let measured_ns = measured.expect("a working clock must produce a measurement");
        m.record_interval(measured);
        assert_eq!(
            m.occupancy_ns(),
            measured_ns,
            "the measured interval is recorded"
        );
    }

    #[test]
    fn the_clock_fault_injection_does_not_escape_its_thread() {
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));
        assert_eq!(try_read_occupancy_ns(), None, "the fault is injected here");
        let elsewhere = std::thread::spawn(try_read_occupancy_ns)
            .join()
            .expect("thread");
        assert!(
            elsewhere.is_some(),
            "the injected fault leaked into another thread"
        );
        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
    }

    #[test]
    fn a_monotonic_clock_failure_remains_missing() {
        FORCE_CLOCK_FAILURE.with(|failure| failure.set(true));
        let measured = try_read_mono_ns();
        FORCE_CLOCK_FAILURE.with(|failure| failure.set(false));
        assert_eq!(
            measured, None,
            "a failed monotonic read must not become a timestamp"
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
}
