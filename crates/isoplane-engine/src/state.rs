//! Per-instance store data: the bindings, the frozen clock, the meter, and
//! the wasm-frame depth capture eligibility is checked against.

use std::time::Instant;

use wasmtime::{Memory, ResourceLimiter};

use crate::bindings::{Bindings, HostBindings};
use crate::pal::platform_files::PlatformFiles;
use crate::EngineConfig;

/// Why the engine terminated a dispatch. Recorded in the store rather than
/// inferred from the error chain, so classification is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kill {
    /// Worker occupancy passed B+G and the epoch callback trapped. There is no
    /// `Clean` variant: a clean kill is the guest returning, and leaves a
    /// `grace_at_ns` record rather than an error.
    OccupancyHard,
    /// The independent elapsed-time backstop for one invocation. Bypasses grace.
    Wall,
}

/// The occupancy budget state the epoch callback consults.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meter {
    pub occupancy0_ns: u64,
    pub wall0: Option<Instant>,
    /// B. Crossing it enters grace.
    pub occupancy_budget_ns: u64,
    /// G. `occupancy_budget_ns + grace_ns` is the only deadline that traps.
    pub grace_ns: u64,
    /// Worker occupancy burned by earlier slices of the same dispatch; B and G are
    /// per dispatch, not per slice.
    pub spent_ns: u64,
    pub wall_cap_ns: u64,
    /// How far to extend the epoch deadline at each check.
    pub check_ticks: u64,
}

impl Meter {
    #[inline]
    pub fn dispatch_occupancy_ns(&self, now_ns: u64) -> u64 {
        self.spent_ns
            .saturating_add(now_ns.saturating_sub(self.occupancy0_ns))
    }

    pub fn remaining_budget_ns(&self, now_ns: u64) -> i64 {
        let remaining =
            i128::from(self.occupancy_budget_ns) - i128::from(self.dispatch_occupancy_ns(now_ns));
        let quantum = i128::from(isoplane_abi::pal::CLOCK_QUANTUM_NS);
        // Preserve the cancellation boundary without exposing a fine wall clock.
        let coarse = if remaining > 0 {
            ((remaining + quantum - 1) / quantum) * quantum
        } else {
            remaining.div_euclid(quantum) * quantum
        };
        coarse.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    #[inline]
    pub fn hard_ns(&self) -> u64 {
        self.occupancy_budget_ns.saturating_add(self.grace_ns)
    }
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            occupancy0_ns: 0,
            wall0: None,
            occupancy_budget_ns: u64::MAX,
            grace_ns: 0,
            spent_ns: 0,
            wall_cap_ns: u64::MAX,
            check_ticks: 1,
        }
    }
}

pub(crate) struct Limits {
    pub max_memory_bytes: usize,
    pub max_tables: usize,
    pub max_table_elements: usize,
    pub oom: bool,
}

impl Limits {
    fn new(cfg: &EngineConfig) -> Self {
        Self {
            max_memory_bytes: cfg.max_memory_bytes as usize,
            max_tables: cfg.max_tables_per_module as usize,
            max_table_elements: cfg.table_elements,
            oom: false,
        }
    }
}

impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if let Some(maximum) = maximum {
            self.max_memory_bytes = self.max_memory_bytes.min(maximum);
        }
        let ok = desired <= self.max_memory_bytes;
        if !ok {
            // `memory.grow` returns -1 rather than trapping, so the fault
            // surfaces later; remembered so it classifies as `OutOfMemory`.
            self.oom = true;
        }
        Ok(ok)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.max_table_elements)
    }

    fn memories(&self) -> usize {
        1
    }
    fn tables(&self) -> usize {
        self.max_tables
    }
    fn instances(&self) -> usize {
        1
    }
}

pub(crate) struct StoreState {
    /// `Option` so the engine can move the box out for the duration of an
    /// import call: `HostBindings` methods take `&mut self` while the payload
    /// borrows the scratch buffer in this same struct.
    pub bindings: Option<Bindings>,
    pub init_pal_calls: std::collections::BTreeMap<&'static str, u32>,
    pub memory: Option<Memory>,
    /// The coarse clock, frozen for the slice. With no threads, no shared
    /// memory and no fine clock, the guest has no timing primitive to build a
    /// Spectre-class gadget on.
    pub clock_ms: u64,
    /// Wasm frames on this thread's native stack, from wasmtime's call hook.
    pub wasm_depth: i32,
    pub meter: Meter,
    pub kill: Option<Kill>,
    /// `Some(occupancy_ns)` once the dispatch crossed B and the host was told. A
    /// deferral on the unsafe-to-trap bit leaves it `None` so the next tick
    /// tries again.
    pub grace_at_ns: Option<u64>,
    /// The runtime layer's unsafe-to-trap bit. Grace entry defers while it is
    /// set; the hard deadline ignores it.
    pub unsafe_to_trap: bool,
    pub grace_deferrals: u64,
    /// The import that violated capability policy.
    pub violation: Option<&'static str>,
    pub abort_msg: Option<String>,
    pub limits: Limits,
    /// Reused across import calls so the copy-out path does not allocate.
    pub scratch: Vec<u8>,
    /// This Store's host platform files and immutable application content.
    pub platform_files: PlatformFiles,
}

impl StoreState {
    pub(crate) fn new(bindings: Box<dyn HostBindings>, cfg: &EngineConfig) -> Self {
        Self::with_bindings(Bindings::Runtime(bindings), cfg)
    }

    pub(crate) fn with_bindings(bindings: Bindings, cfg: &EngineConfig) -> Self {
        let memory_limit = match &bindings {
            Bindings::Runtime(runtime) => {
                runtime.memory_limit_bytes().unwrap_or(cfg.max_memory_bytes)
            }
            Bindings::Initialization(bindings) => bindings
                .memory_limit_bytes()
                .unwrap_or(cfg.max_memory_bytes),
        };
        let mut state = Self::unbound(cfg, memory_limit);
        state.bindings = Some(bindings);
        state
    }

    pub(crate) fn unbound(cfg: &EngineConfig, memory_limit: u64) -> Self {
        let mut limits = Limits::new(cfg);
        limits.max_memory_bytes = memory_limit.min(cfg.max_memory_bytes) as usize;
        Self {
            bindings: None,
            init_pal_calls: std::collections::BTreeMap::new(),
            memory: None,
            clock_ms: 0,
            wasm_depth: 0,
            meter: Meter::default(),
            kill: None,
            grace_at_ns: None,
            unsafe_to_trap: false,
            grace_deferrals: 0,
            violation: None,
            abort_msg: None,
            limits,
            scratch: Vec::new(),
            platform_files: cfg.platform_files.clone(),
        }
    }

    pub(crate) fn check_occupancy(&mut self, now_ns: u64) -> Result<(), BudgetExceeded> {
        let occupancy_ns = self.meter.dispatch_occupancy_ns(now_ns);
        if occupancy_ns >= self.meter.hard_ns() {
            self.kill = Some(Kill::OccupancyHard);
            return Err(BudgetExceeded {
                occupancy_ns,
                budget_ns: self.meter.occupancy_budget_ns,
                grace_ns: self.meter.grace_ns,
            });
        }
        if occupancy_ns >= self.meter.occupancy_budget_ns && self.grace_at_ns.is_none() {
            if self.unsafe_to_trap {
                self.grace_deferrals += 1;
            } else if let Some(bindings) = self.bindings.as_mut() {
                bindings.enter_grace();
                self.grace_at_ns = Some(occupancy_ns);
            }
        }
        Ok(())
    }

    pub(crate) fn reset_execution(&mut self) {
        self.memory = None;
        self.wasm_depth = 0;
        self.kill = None;
        self.grace_at_ns = None;
        self.unsafe_to_trap = false;
        self.grace_deferrals = 0;
        self.violation = None;
        self.abort_msg = None;
        self.limits.oom = false;
        self.scratch.clear();
    }

    pub(crate) fn begin_slice(&mut self, clock_ms: u64, meter: Meter) {
        self.clock_ms = clock_ms;
        self.meter = meter;
        self.kill = None;
        self.grace_at_ns = None;
        // The bit describes the runtime layer's current critical section; a
        // slice that ended inside one ended by trapping.
        self.unsafe_to_trap = false;
        self.grace_deferrals = 0;
        self.violation = None;
        self.abort_msg = None;
        self.limits.oom = false;
    }

    pub(crate) fn is_init_sandbox(&self) -> bool {
        self.bindings
            .as_ref()
            .map(|b| b.is_init_sandbox())
            .unwrap_or(false)
    }

    pub(crate) fn init_runtime_capability(&self, name: &str) -> bool {
        if name == "entropy_fill" {
            return self
                .bindings
                .as_ref()
                .is_some_and(|bindings| bindings.permits_initialization_entropy());
        }

        (isoplane_abi::imports::RUNTIME_INIT_CAPABILITIES.contains(&name)
            || (matches!(name, "socket_call" | "poll_call")
                && self
                    .bindings
                    .as_ref()
                    .is_some_and(|bindings| bindings.permits_initialization_io())))
            && self
                .bindings
                .as_ref()
                .is_some_and(|b| b.init_runtime_bringup())
    }
}

/// The hard kill, raised when an epoch callback observes B+G or more. The message
/// names both numbers so a tenant can tell "my code is slow" from "my code
/// cannot be interrupted".
#[derive(Debug, thiserror::Error)]
#[error(
    "occupancy budget exceeded: {occupancy_ns} ns of worker occupancy against a {budget_ns} ns budget \
     (+{grace_ns} ns grace); the dispatch was still executing when the hard deadline was checked and was \
     terminated without unwinding"
)]
pub(crate) struct BudgetExceeded {
    pub occupancy_ns: u64,
    pub budget_ns: u64,
    pub grace_ns: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("wall-clock backstop tripped after {wall_ns} ns")]
pub(crate) struct WallCapExceeded {
    pub wall_ns: u64,
}

/// An import the init sandbox forbids, which is how an impure static
/// initialiser is detected.
#[derive(Debug, thiserror::Error)]
#[error("capability violation: import `{import}` is not reachable here (sealed init sandbox: {init_sandbox})")]
pub(crate) struct CapabilityViolation {
    pub import: &'static str,
    pub init_sandbox: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("guest aborted: {message}")]
pub(crate) struct GuestAbort {
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("isoplane_alloc({size}) returned 0: guest heap exhausted")]
pub(crate) struct AllocFailed {
    pub size: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("reentrant call into an instance with {depth} wasm frames already on the stack")]
pub(crate) struct Reentrancy {
    pub depth: i32,
}

#[cfg(test)]
mod resource_limit_tests {
    use super::*;

    #[test]
    fn table_growth_obeys_the_same_limit_in_both_allocator_modes() {
        let wasm = wat::parse_str("(module (table 2 funcref) (func (export \"grow\") (result i32) ref.null func i32.const 1 table.grow))").expect("table growth fixture must assemble");
        for pooling in [false, true] {
            for (table_elements, expected) in [(2, -1), (3, 2)] {
                let config = EngineConfig {
                    pooling,
                    table_elements,
                    max_instances: 2,
                    max_memory_bytes: 65536,
                    ..Default::default()
                };
                let engine =
                    crate::wtconfig::build_engine(&config).expect("table policy must configure");
                let module =
                    wasmtime::Module::new(&engine, &wasm).expect("table fixture must compile");
                let mut store = wasmtime::Store::new(&engine, Limits::new(&config));
                store.limiter(|limits| limits);
                store.set_epoch_deadline(u64::MAX);
                let instance = wasmtime::Instance::new(&mut store, &module, &[])
                    .expect("initial table must fit");
                let grow = instance
                    .get_typed_func::<(), i32>(&mut store, "grow")
                    .expect("fixture exports growth");
                assert_eq!(
                    grow.call(&mut store, ())
                        .expect("growth refusal returns minus one"),
                    expected,
                    "configured table growth must be enforced with pooling={pooling}"
                );
            }
        }
    }
}

#[cfg(test)]
mod occupancy_tests {
    use super::*;
    use crate::ImportCall;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct GraceCalls(Arc<AtomicUsize>);

    impl HostBindings for GraceCalls {
        fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
            panic!("budget checks do not call guest imports")
        }
        fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
            panic!("budget checks do not call guest imports")
        }
        fn slice_clock_ms(&self) -> u64 {
            0
        }
        fn enter_grace(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn state() -> (StoreState, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut state = StoreState::new(
            Box::new(GraceCalls(calls.clone())),
            &EngineConfig::default(),
        );
        state.meter = Meter {
            occupancy0_ns: 1_000,
            occupancy_budget_ns: 100,
            grace_ns: 20,
            spent_ns: 30,
            ..Meter::default()
        };
        (state, calls)
    }

    #[test]
    fn guest_budget_reads_are_coarse_and_preserve_the_cancellation_boundary() {
        let meter = Meter {
            occupancy0_ns: 100,
            occupancy_budget_ns: 2_000_000,
            ..Meter::default()
        };
        for (elapsed, expected) in [
            (0, 2_000_000),
            (1, 2_000_000),
            (999_999, 2_000_000),
            (1_000_000, 1_000_000),
            (1_999_999, 1_000_000),
            (2_000_000, 0),
            (2_000_001, -1_000_000),
        ] {
            assert_eq!(meter.remaining_budget_ns(100 + elapsed), expected);
        }
    }

    #[test]
    fn prior_slices_count_toward_grace_and_grace_is_delivered_once() {
        let (mut state, calls) = state();
        state.check_occupancy(1_069).expect("below budget");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        state.check_occupancy(1_070).expect("budget enters grace");
        state.check_occupancy(1_089).expect("still in grace");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.grace_at_ns, Some(100));
        let error = state.check_occupancy(1_090).expect_err("hard deadline");
        assert_eq!(error.occupancy_ns, 120);
        assert_eq!(state.kill, Some(Kill::OccupancyHard));
    }

    #[test]
    fn unsafe_runtime_work_defers_grace_but_cannot_defer_the_hard_kill() {
        let (mut state, calls) = state();
        state.unsafe_to_trap = true;
        state.check_occupancy(1_070).expect("grace defers");
        assert_eq!(state.grace_deferrals, 1);
        assert_eq!(state.grace_at_ns, None);
        state
            .check_occupancy(1_090)
            .expect_err("pinned bit cannot prevent hard kill");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(state.kill, Some(Kill::OccupancyHard));
    }

    #[test]
    fn clearing_the_unsafe_bit_allows_the_next_check_to_deliver_grace() {
        let (mut state, calls) = state();
        state.unsafe_to_trap = true;
        state.check_occupancy(1_070).expect("grace defers");
        state.unsafe_to_trap = false;
        state.check_occupancy(1_071).expect("grace can now enter");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(state.grace_at_ns, Some(101));
    }

    #[test]
    fn a_delayed_check_can_skip_grace_without_granting_a_fresh_budget() {
        let (mut state, calls) = state();
        let error = state
            .check_occupancy(10_000)
            .expect_err("late check still enforces budget");
        assert_eq!(error.occupancy_ns, 9_030);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(state.kill, Some(Kill::OccupancyHard));
    }
}
