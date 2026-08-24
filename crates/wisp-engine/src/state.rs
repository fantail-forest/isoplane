//! Per-instance store data: the bindings, the frozen clock, the meter, and the
//! wasm-frame depth that makes "checked, not assumed" (§6.6) true.

use std::time::Instant;

use wasmtime::{Memory, ResourceLimiter};

use crate::bindings::HostBindings;

/// Why the engine terminated a dispatch. Recorded in the store rather than
/// inferred from the error chain, so classification is exact instead of
/// string-matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kill {
    /// §6.4: `CLOCK_THREAD_CPUTIME_ID` passed the dispatch's CPU budget and the
    /// epoch counter delivered the kill.
    Cpu,
    /// The wall-clock backstop. Never a meter (§11.2) — purely a guard against
    /// a dispatch pinned off-CPU forever.
    Wall,
}

/// The CPU budget state the epoch callback consults. This is the mechanism
/// §4.5 names: *"the CPU-budget kill (thread-clock metered, epoch-delivered)"* —
/// epochs are wall-clock ticks, so they cannot themselves express a CPU budget;
/// they exist to give us a periodic, guest-preempting callback in which the
/// thread clock is the thing actually compared.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meter {
    pub cpu0_ns: u64,
    pub wall0: Option<Instant>,
    pub cpu_budget_ns: u64,
    pub wall_cap_ns: u64,
    /// How far to extend the epoch deadline at each check.
    pub check_ticks: u64,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            cpu0_ns: 0,
            wall0: None,
            cpu_budget_ns: u64::MAX,
            wall_cap_ns: u64::MAX,
            check_ticks: 1,
        }
    }
}

/// §5.5, §11.1: exactly one linear memory, maximum = the plan ceiling.
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
    ) -> anyhow::Result<bool> {
        let ok = desired <= self.max_memory_bytes;
        if !ok {
            // Remembered so a later trap can be classified `OutOfMemory`
            // instead of the generic `Sandbox`: `memory.grow` itself returns
            // -1 rather than trapping, so the fault surfaces further along.
            self.oom = true;
        }
        Ok(ok)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
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
    /// `Option` only so the engine can move the box out for the duration of an
    /// import call — `HostBindings` methods take `&mut self` while the encoded
    /// payload borrows the scratch buffer that lives in this same struct.
    pub bindings: Option<Box<dyn HostBindings>>,
    /// Set immediately after instantiation; the import path needs it and a
    /// per-call `get_export("memory")` lookup would be pure waste.
    pub memory: Option<Memory>,

    /// §8.5: the coarse clock, **frozen** for the slice. This is a security
    /// control, not a convenience: with no threads, no shared memory and no
    /// fine clock, the guest has no measurement primitive to build a
    /// Spectre-class gadget on (§8.4).
    pub clock_ms: u64,

    /// Number of wasm frames currently on this thread's native stack, as
    /// reported by wasmtime's own call hook — not by our bookkeeping around the
    /// call. §6.6 requires snapshot eligibility to be *checked*, and this is
    /// the thing that gets checked.
    pub wasm_depth: i32,
    /// High-water mark, kept so tests can prove the counter tracks reality in
    /// both directions and so a §5.4 reentrancy violation would be visible.
    pub max_wasm_depth: i32,

    pub meter: Meter,
    pub kill: Option<Kill>,
    /// The import that violated capability policy, for `TrapInfo::message` and
    /// for §4.4's deferral loop, which needs to know *which* import an impure
    /// static initializer touched.
    pub violation: Option<&'static str>,
    /// Message passed to `abort` (§5.3 core group).
    pub abort_msg: Option<String>,

    pub limits: Limits,
    /// Reused across import calls so the copy-out path does not allocate.
    pub scratch: Vec<u8>,
}

impl StoreState {
    pub(crate) fn new(bindings: Box<dyn HostBindings>, max_memory_bytes: usize) -> Self {
        Self {
            bindings: Some(bindings),
            memory: None,
            clock_ms: 0,
            wasm_depth: 0,
            max_wasm_depth: 0,
            meter: Meter::default(),
            kill: None,
            violation: None,
            abort_msg: None,
            limits: Limits { max_memory_bytes, oom: false },
            scratch: Vec::new(),
        }
    }

    /// Clear the per-dispatch scratch state, keeping the bindings and the
    /// allocation capacity.
    pub(crate) fn begin_slice(&mut self, clock_ms: u64, meter: Meter) {
        self.clock_ms = clock_ms;
        self.meter = meter;
        self.kill = None;
        self.violation = None;
        self.abort_msg = None;
        self.limits.oom = false;
    }

    pub(crate) fn is_init_sandbox(&self) -> bool {
        self.bindings.as_ref().map(|b| b.is_init_sandbox()).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Typed errors raised out of the engine's own code, so trap classification is
// exact rather than a match on wasmtime's message strings.
// ---------------------------------------------------------------------------

/// §6.4 budget kill. Raised from the epoch deadline callback.
#[derive(Debug, thiserror::Error)]
#[error("cpu budget exceeded: {cpu_ns} ns of thread CPU against a {budget_ns} ns budget")]
pub(crate) struct BudgetExceeded {
    pub cpu_ns: u64,
    pub budget_ns: u64,
}

/// §6.4 wall-clock backstop. Distinct from `BudgetExceeded` because it is not a
/// CPU-budget event.
#[derive(Debug, thiserror::Error)]
#[error("wall-clock backstop tripped after {wall_ns} ns")]
pub(crate) struct WallCapExceeded {
    pub wall_ns: u64,
}

/// §4.4 / invariant 4. Either an import the module did not declare (which
/// normally cannot happen — the linker defines only [`wisp_abi::imports::IMPORTS`]
/// — but is checked twice on purpose) or one that the sealed init sandbox
/// forbids, which is the signal that a static initializer is impure.
#[derive(Debug, thiserror::Error)]
#[error("capability violation: import `{import}` is not reachable here (sealed init sandbox: {init_sandbox})")]
pub(crate) struct CapabilityViolation {
    pub import: &'static str,
    pub init_sandbox: bool,
}

/// The guest called `abort` (§5.3).
#[derive(Debug, thiserror::Error)]
#[error("guest aborted: {message}")]
pub(crate) struct GuestAbort {
    pub message: String,
}

/// `wisp_alloc` could not satisfy a host->guest buffer handoff (§5.5).
#[derive(Debug, thiserror::Error)]
#[error("wisp_alloc({size}) returned 0: guest heap exhausted")]
pub(crate) struct AllocFailed {
    pub size: usize,
}

/// §5.4: "the host MUST NOT call into an instance while a previous call into it
/// has not returned". Enforced here as well as in `wisp-host` because a
/// violation would corrupt the meter and the frozen clock, not merely the
/// guest.
#[derive(Debug, thiserror::Error)]
#[error("reentrant call into an instance with {depth} wasm frames already on the stack (§5.4)")]
pub(crate) struct Reentrancy {
    pub depth: i32,
}
