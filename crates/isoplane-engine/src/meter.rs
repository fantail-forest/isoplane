//! Monotonic worker-occupancy accounting across a dispatch's active intervals.
//! Parked intervals are outside the meter; OS preemption and synchronous host
//! work remain inside the occupied worker interval.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Failed clock reads, process-wide, kept distinct from a measured zero.
static CLOCK_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// How many worker-occupancy clock reads have failed in this process. Non-zero means
/// some measured interval is a floor rather than a measurement.
pub fn clock_read_failures() -> u64 {
    CLOCK_READ_FAILURES.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Thread-local so one test's injected fault is not another test's flake.
    static READS_BEFORE_FAILURE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FAIL_INTERVAL_END: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_NOW_NS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
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

/// Monotonic nanoseconds from a process-local origin, or `None` on overflow.
/// Differences measure worker occupancy only when taken around active work.
#[inline]
pub fn try_occupancy_nanos() -> Option<u64> {
    if clock_failure_forced() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    #[cfg(test)]
    if let Some(now) = TEST_NOW_NS.get() {
        return Some(now);
    }
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let measured = u64::try_from(ORIGIN.get_or_init(Instant::now).elapsed().as_nanos()).ok();
    if measured.is_none() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    measured
}

/// End-point reading for a returned slice; an unavailable or regressing clock is incomplete.
#[inline]
pub(crate) fn interval_occupancy_ns(start: u64) -> Option<u64> {
    #[cfg(test)]
    if FAIL_INTERVAL_END.get() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    try_occupancy_nanos().and_then(|end| end.checked_sub(start))
}

#[derive(Debug, thiserror::Error)]
#[error("worker occupancy clock unavailable; guest execution cannot enforce its occupancy budget")]
pub(crate) struct OccupancyClockUnavailable;

/// Refuse execution when a budget check has no trustworthy clock reading.
#[inline]
pub(crate) fn occupancy_nanos() -> Result<u64, OccupancyClockUnavailable> {
    try_occupancy_nanos().ok_or(OccupancyClockUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EngineConfig, HostBindings, ImportCall, InstanceOps, IsoplaneEngine, OccupancyBudget,
    };

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
            TEST_NOW_NS.set(None);
        }
    }

    struct DenyImports;

    impl HostBindings for DenyImports {
        fn begin_slice(&mut self) {}
        fn end_slice(&mut self) {}
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
        instance_with_bindings(imports, initialize, Box::new(DenyImports))
    }

    fn instance_with_bindings(
        imports: &str,
        initialize: &str,
        bindings: Box<dyn HostBindings>,
    ) -> crate::Instance {
        let engine = IsoplaneEngine::new(EngineConfig {
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
              (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 1024)
              (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
              (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0)
              (func (export "isoplane_initialize") (result i64) {initialize} i64.const 0))"#
        ))
        .expect("the clock fixture must assemble");
        let template = engine.load_template(&wasm).expect("the fixture must load");
        engine
            .activate(&template, bindings)
            .expect("the fixture must activate")
    }

    #[test]
    fn synchronous_host_work_counts_as_occupancy_but_time_between_slices_does_not() {
        struct HostWork;
        impl HostBindings for HostWork {
            fn begin_slice(&mut self) {}
            fn end_slice(&mut self) {}
            fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
                TEST_NOW_NS.set(Some(TEST_NOW_NS.get().expect("controlled clock") + 1_000));
                1
            }
            fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
                panic!("unexpected synchronous import {}", call.import);
            }
            fn slice_clock_ms(&self) -> u64 {
                0
            }
        }

        let _reset = ClockFault;
        TEST_NOW_NS.set(Some(100));
        let mut guest = instance_with_bindings(
            r#"(import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
            "i64.const 0 call $timer drop",
            Box::new(HostWork),
        );
        let first = guest.run_initialize(OccupancyBudget::default());
        assert!(first.trap.is_none(), "{:?}", first.trap);
        assert_eq!(first.occupancy_ns, 1_000);
        TEST_NOW_NS.set(Some(1_000_000_000));
        let next = guest.run_initialize(OccupancyBudget::default().after(first.occupancy_ns));
        assert!(next.trap.is_none(), "{:?}", next.trap);
        assert_eq!(next.occupancy_ns, 1_000);
    }

    #[test]
    fn controlled_occupancy_drives_real_guest_grace_exit_and_kill() {
        use crate::{Completion, GraceEnd};
        use std::sync::Arc;

        struct Advance {
            engine: Arc<OnceLock<wasmtime::Engine>>,
            elapsed_ns: u64,
            grace_calls: Arc<AtomicU64>,
        }
        impl HostBindings for Advance {
            fn begin_slice(&mut self) {}
            fn end_slice(&mut self) {}
            fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
                TEST_NOW_NS.set(Some(100 + self.elapsed_ns));
                self.engine
                    .get()
                    .expect("engine installed before entry")
                    .increment_epoch();
                1
            }
            fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
                panic!("unexpected import {}", call.import)
            }
            fn slice_clock_ms(&self) -> u64 {
                0
            }
            fn enter_grace(&mut self) {
                self.grace_calls.fetch_add(1, Ordering::Relaxed);
            }
        }

        let _reset = ClockFault;
        let budget = OccupancyBudget::default();
        for (elapsed_ns, completion, phase, grace_calls) in [
            (
                budget.budget_ns() + 1,
                Completion::BudgetExit,
                GraceEnd::Exited,
                1,
            ),
            (
                budget.hard_ns() + 1,
                Completion::BudgetKill,
                GraceEnd::Killed,
                0,
            ),
        ] {
            TEST_NOW_NS.set(Some(100));
            let engine = Arc::new(OnceLock::new());
            let observed_grace = Arc::new(AtomicU64::new(0));
            let mut guest = instance_with_bindings(
                r#"(import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
                // The host call moves the occupancy clock past B (or B+G); a
                // short loop then gives the epoch callback a check to land on
                // before the guest returns, as it would mid-computation.
                r#"i64.const 0 call $timer drop
                    (loop $work
                        (i32.store (i32.const 4) (i32.add (i32.load (i32.const 4)) (i32.const 1)))
                        (br_if $work (i32.lt_u (i32.load (i32.const 4)) (i32.const 1000))))
                    i32.const 0 i32.const 1 i32.store"#,
                Box::new(Advance {
                    engine: engine.clone(),
                    elapsed_ns,
                    grace_calls: observed_grace.clone(),
                }),
            );
            engine
                .set(guest.engine.wasmtime.clone())
                .expect("install engine once");
            let outcome = guest.run_initialize(budget);
            assert_eq!(outcome.completion, completion, "{:?}", outcome.trap);
            assert_eq!(outcome.grace_end, Some(phase));
            assert_eq!(outcome.occupancy_ns, elapsed_ns);
            assert_eq!(observed_grace.load(Ordering::Relaxed), grace_calls);
            if phase == GraceEnd::Exited {
                assert_eq!(guest.memory()[0], 1, "cooperative cleanup executed");
                assert!(guest.discarded.is_none(), "an exit keeps the instance");
            } else {
                assert!(guest.discarded.is_some(), "a kill discards the instance");
            }
        }
    }

    #[test]
    fn an_unreadable_clock_is_counted_rather_than_read_as_zero() {
        let before = clock_read_failures();
        let fault = ClockFault::after(0);
        assert_eq!(
            try_occupancy_nanos(),
            None,
            "an unreadable clock must report failure"
        );
        assert!(
            clock_read_failures() > before,
            "the failed read must be counted"
        );
        let elsewhere = std::thread::spawn(try_occupancy_nanos)
            .join()
            .expect("the probe thread must not panic");
        assert!(
            elsewhere.is_some(),
            "the injected fault must not affect another thread"
        );
        drop(fault);
        assert!(
            try_occupancy_nanos().is_some(),
            "clock reads must recover after the fault"
        );
    }

    #[test]
    fn an_unreadable_start_clock_refuses_guest_entry() {
        let mut guest = instance("", "i32.const 0 i32.const 1 i32.store");
        let _fault = ClockFault::after(0);
        let outcome = guest.run_initialize(OccupancyBudget::default());
        assert_eq!(
            outcome.completion,
            crate::Completion::Trapped,
            "guest entry needs a readable occupancy clock"
        );
        assert_eq!(
            guest.memory()[0],
            0,
            "the refused slice must not execute guest code"
        );
        assert_eq!(
            outcome.occupancy_measurement,
            crate::OccupancyMeasurement::NotExecuted,
            "start-clock refusal must distinguish no guest entry from an unmeasured slice"
        );
    }

    #[test]
    fn an_unreadable_epoch_clock_stops_a_guest_without_claiming_budget_exhaustion() {
        let mut guest = instance("", "(loop $spin br $spin)");
        let _fault = ClockFault::after(1);
        let outcome = guest.run_initialize(OccupancyBudget {
            slice_wall_ms_cap: 10,
            ..OccupancyBudget::default()
        });
        let trap = outcome.trap.expect("the unreadable epoch clock must trap");
        assert_eq!(
            outcome.completion,
            crate::Completion::Trapped,
            "clock failure must end execution as a host fault"
        );
        assert_eq!(
            trap.kind,
            crate::TrapKind::OccupancyClockUnavailable,
            "a missing clock must preserve its typed host-fault reason"
        );
        assert!(
            guest.discarded.is_some(),
            "execution interrupted by a clock failure must be discarded"
        );
    }

    #[test]
    fn an_unreadable_end_clock_marks_the_returned_occupancy_floor_incomplete() {
        let mut guest = instance("", "i32.const 0 i32.const 1 i32.store");
        let _fault = ClockFault;
        FAIL_INTERVAL_END.set(true);
        let outcome = guest.run_initialize(OccupancyBudget::default());
        assert_eq!(
            guest.memory()[0],
            1,
            "the end-clock fixture must actually execute guest code"
        );
        assert_eq!(
            outcome.occupancy_measurement,
            crate::OccupancyMeasurement::Incomplete,
            "an unavailable end reading must not certify a complete zero occupancy measurement"
        );
    }

    #[test]
    fn a_host_panic_releases_the_engine_watchdog_hot_count() {
        let mut guest = instance(
            r#"(import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
            "i64.const 0 call $timer drop",
        );
        let watchdog = guest.engine.watchdog.shared().clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            guest.run_initialize(OccupancyBudget::default())
        }));
        assert!(
            result.is_err(),
            "the real imported host call must exercise unwinding"
        );
        assert_eq!(watchdog.hot_count(), 0, "a caught host panic must not leave the engine watchdog believing guest execution is still hot");
    }
}
