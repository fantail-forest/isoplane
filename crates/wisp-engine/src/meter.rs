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
    static READS_BEFORE_FAILURE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FAIL_INTERVAL_END: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn clock_failure_forced() -> bool {
    READS_BEFORE_FAILURE.with(|reads| match reads.get() {
        None => false,
        Some(0) => true,
        Some(remaining) => {
            reads.set(Some(remaining - 1));
            false
        }
    })
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

/// End-point reading for a returned invocation; an unavailable or regressing clock is incomplete.
#[inline]
pub(crate) fn interval_cpu_ns(start: u64) -> Option<u64> {
    #[cfg(test)]
    if FAIL_INTERVAL_END.get() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    try_thread_cpu_nanos().and_then(|end| end.checked_sub(start))
}

#[derive(Debug, thiserror::Error)]
#[error("thread CPU clock unavailable; guest execution cannot enforce its CPU budget")]
pub(crate) struct CpuClockUnavailable;

/// Refuse execution when a budget check has no trustworthy clock reading.
#[inline]
pub(crate) fn thread_cpu_nanos() -> Result<u64, CpuClockUnavailable> {
    try_thread_cpu_nanos().ok_or(CpuClockUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CpuBudget, EngineConfig, HostBindings, ImportCall, InstanceOps, WispEngine};

    struct ClockFault;

    impl ClockFault {
        fn after(reads: usize) -> Self {
            READS_BEFORE_FAILURE.with(|remaining| remaining.set(Some(reads)));
            Self
        }
    }

    impl Drop for ClockFault {
        fn drop(&mut self) {
            READS_BEFORE_FAILURE.with(|remaining| remaining.set(None));
            FAIL_INTERVAL_END.set(false);
        }
    }

    struct DenyImports;

    impl HostBindings for DenyImports {
        fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
            panic!("clock fixture unexpectedly called {}", call.import);
        }

        fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
            panic!("clock fixture unexpectedly called {}", call.import);
        }

        fn slice_clock_ms(&self) -> u64 {
            0
        }
    }

    fn instance(imports: &str, initialize: &str) -> crate::Instance {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 2,
            max_memory_bytes: 65536,
            guard_size_bytes: 65536,
            ..EngineConfig::default()
        })
        .expect("the clock fixture engine must start");
        let wasm = wat::parse_str(format!(
            r#"(module
              {imports}
              (memory (export "memory") 1 1)
              (func (export "wisp_alloc") (param i32 i32) (result i32) i32.const 1024)
              (func (export "wisp_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "wisp_wake") (param i64 i32 i32) (result i64) i64.const 0)
              (func (export "wisp_initialize") {initialize}))"#
        ))
        .expect("the clock fixture must assemble");
        let template = engine.load_template(&wasm).expect("the fixture must load");
        engine
            .activate(&template, Box::new(DenyImports))
            .expect("the fixture must activate")
    }

    #[test]
    fn an_unreadable_clock_is_counted_rather_than_read_as_zero() {
        let before = clock_read_failures();
        let fault = ClockFault::after(0);
        assert_eq!(
            try_thread_cpu_nanos(),
            None,
            "an unreadable clock must report failure"
        );
        assert!(
            clock_read_failures() > before,
            "the failed read must be counted"
        );
        let elsewhere = std::thread::spawn(try_thread_cpu_nanos)
            .join()
            .expect("the probe thread must not panic");
        assert!(
            elsewhere.is_some(),
            "the injected fault must not affect another thread"
        );
        drop(fault);
        assert!(
            try_thread_cpu_nanos().is_some(),
            "clock reads must recover after the fault"
        );
    }

    #[test]
    fn the_native_thread_clock_measures_cpu_work() {
        wisp_fixtures::assert_cpu_clock_advances(try_thread_cpu_nanos);
    }

    #[test]
    fn an_unreadable_start_clock_refuses_guest_entry() {
        let mut guest = instance("", "i32.const 0 i32.const 1 i32.store");
        let _fault = ClockFault::after(0);
        let outcome = guest.run_initialize(CpuBudget::http());
        assert_eq!(
            outcome.completion,
            crate::Completion::Trapped,
            "guest entry needs a readable CPU clock"
        );
        assert_eq!(
            guest.memory()[0],
            0,
            "the refused invocation must not execute guest code"
        );
        assert_eq!(
            outcome.cpu_measurement,
            crate::CpuMeasurement::NotExecuted,
            "start-clock refusal must distinguish no guest entry from an unmeasured invocation"
        );
    }

    #[test]
    fn an_unreadable_budget_clock_traps_instead_of_granting_more_cpu() {
        let mut guest = instance(
            r#"(import "wisp" "budget_remaining_ns" (func $budget (result i64)))"#,
            "call $budget drop i32.const 0 i32.const 1 i32.store",
        );
        let _fault = ClockFault::after(0);
        let error = crate::call::call_entry(
            guest.live.as_mut().expect("the fixture has a live store"),
            crate::instance::Entry::Initialize,
        )
        .expect_err("an unreadable budget import must trap without relying on the watchdog");
        assert!(
            error.downcast_ref::<CpuClockUnavailable>().is_some(),
            "the import must preserve its typed clock error"
        );
        assert_eq!(
            guest.memory()[0],
            0,
            "the guest must not continue after an unreadable budget"
        );
    }

    #[test]
    fn an_unreadable_epoch_clock_stops_a_guest_without_claiming_budget_exhaustion() {
        let mut guest = instance("", "(loop $spin br $spin)");
        let _fault = ClockFault::after(1);
        let outcome = guest.run_initialize(CpuBudget {
            wall_ms_cap: 10,
            ..CpuBudget::http()
        });
        let trap = outcome.trap.expect("the unreadable epoch clock must trap");
        assert_eq!(
            outcome.completion,
            crate::Completion::Trapped,
            "clock failure must end execution as a host fault"
        );
        assert_eq!(
            trap.kind,
            crate::TrapKind::CpuClockUnavailable,
            "a missing clock must preserve its typed host-fault reason"
        );
        assert!(
            guest.reset().is_err(),
            "execution interrupted by a clock failure must be tombstoned"
        );
    }

    #[test]
    fn an_unreadable_end_clock_marks_the_returned_cpu_floor_incomplete() {
        let mut guest = instance("", "i32.const 0 i32.const 1 i32.store");
        let _fault = ClockFault;
        FAIL_INTERVAL_END.set(true);
        let outcome = guest.run_initialize(CpuBudget::http());
        assert_eq!(
            guest.memory()[0],
            1,
            "the end-clock fixture must actually execute guest code"
        );
        assert_eq!(
            outcome.cpu_measurement,
            crate::CpuMeasurement::Incomplete,
            "an unavailable end reading must not certify a complete zero CPU measurement"
        );
    }

    #[test]
    fn a_host_panic_releases_the_engine_watchdog_hot_count() {
        let mut guest = instance(
            r#"(import "wisp" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
            "i64.const 0 call $timer drop",
        );
        let watchdog = guest.engine.watchdog.shared().clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guest.run_initialize(CpuBudget::http())
        }));
        assert!(
            result.is_err(),
            "the real imported host call must exercise unwinding"
        );
        assert_eq!(watchdog.hot_count(), 0, "a caught host panic must not leave the engine watchdog believing guest execution is still hot");
    }
}
