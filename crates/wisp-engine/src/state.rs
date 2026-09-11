//! Per-instance store data: the bindings, the frozen clock, the meter, and
//! the wasm-frame depth capture eligibility is checked against.

use std::time::Instant;

use wasmtime::{Memory, ResourceLimiter};

use crate::bindings::HostBindings;
use crate::pal::platform_files::PlatformFiles;
use crate::EngineConfig;

/// Why the engine terminated a dispatch. Recorded in the store rather than
/// inferred from the error chain, so classification is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kill {
    /// Thread CPU passed B+G and the epoch callback trapped. There is no
    /// `Clean` variant: a clean kill is the guest returning, and leaves a
    /// `grace_at_ns` record rather than an error.
    CpuHard,
    /// The wall-clock backstop against a dispatch pinned off-CPU. Bypasses
    /// grace, since a dispatch that is not running has nothing to unwind.
    Wall,
}

/// The CPU budget state the epoch callback consults.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meter {
    pub cpu0_ns: u64,
    pub wall0: Option<Instant>,
    /// B. Crossing it enters grace.
    pub cpu_budget_ns: u64,
    /// G. `cpu_budget_ns + grace_ns` is the only deadline that traps.
    pub grace_ns: u64,
    /// Thread CPU burned by earlier slices of the same dispatch; B and G are
    /// per dispatch, not per slice.
    pub spent_ns: u64,
    pub wall_cap_ns: u64,
    /// How far to extend the epoch deadline at each check.
    pub check_ticks: u64,
}

impl Meter {
    #[inline]
    pub fn dispatch_cpu_ns(&self, now_ns: u64) -> u64 {
        self.spent_ns
            .saturating_add(now_ns.saturating_sub(self.cpu0_ns))
    }

    #[inline]
    pub fn hard_ns(&self) -> u64 {
        self.cpu_budget_ns.saturating_add(self.grace_ns)
    }
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            cpu0_ns: 0,
            wall0: None,
            cpu_budget_ns: u64::MAX,
            grace_ns: 0,
            spent_ns: 0,
            wall_cap_ns: u64::MAX,
            check_ticks: 1,
        }
    }
}

pub(crate) struct Limits {
    pub max_memory_bytes: usize,
    pub oom: bool,
}

impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
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
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }

    fn memories(&self) -> usize {
        1
    }
    fn tables(&self) -> usize {
        16
    }
    fn instances(&self) -> usize {
        1
    }
}

pub(crate) struct StoreState {
    /// `Option` so the engine can move the box out for the duration of an
    /// import call: `HostBindings` methods take `&mut self` while the payload
    /// borrows the scratch buffer in this same struct.
    pub bindings: Option<Box<dyn HostBindings>>,
    pub memory: Option<Memory>,
    /// The coarse clock, frozen for the slice. With no threads, no shared
    /// memory and no fine clock, the guest has no timing primitive to build a
    /// Spectre-class gadget on.
    pub clock_ms: u64,
    /// Wasm frames on this thread's native stack, from wasmtime's call hook.
    pub wasm_depth: i32,
    pub meter: Meter,
    pub kill: Option<Kill>,
    /// `Some(cpu_ns)` once the dispatch crossed B and the host was told. A
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
    /// The engine's declared read-only files, served by the PAL filesystem.
    pub platform_files: PlatformFiles,
}

impl StoreState {
    pub(crate) fn new(bindings: Box<dyn HostBindings>, cfg: &EngineConfig) -> Self {
        Self {
            bindings: Some(bindings),
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
            limits: Limits {
                max_memory_bytes: cfg.max_memory_bytes as usize,
                oom: false,
            },
            scratch: Vec::new(),
            platform_files: cfg.platform_files.clone(),
        }
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
}

/// The hard kill, raised from the epoch deadline callback at B+G. The message
/// names both numbers so a tenant can tell "my code is slow" from "my code
/// cannot be interrupted".
#[derive(Debug, thiserror::Error)]
#[error(
    "cpu budget exceeded: {cpu_ns} ns of thread CPU against a {budget_ns} ns budget \
     (+{grace_ns} ns grace); the dispatch was still executing at the hard deadline and was \
     terminated without unwinding"
)]
pub(crate) struct BudgetExceeded {
    pub cpu_ns: u64,
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
#[error("wisp_alloc({size}) returned 0: guest heap exhausted")]
pub(crate) struct AllocFailed {
    pub size: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("reentrant call into an instance with {depth} wasm frames already on the stack")]
pub(crate) struct Reentrancy {
    pub depth: i32,
}
