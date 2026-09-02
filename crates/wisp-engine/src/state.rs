//! Per-instance store data: the bindings, the frozen clock, the meter, and the
//! wasm-frame depth that makes snapshot eligibility checked rather than
//! assumed.

use std::time::Instant;

use wasmtime::{Memory, ResourceLimiter};

use crate::bindings::HostBindings;

/// Why the engine terminated a dispatch. Recorded in the store rather than
/// inferred from the error chain, so classification is exact instead of
/// string-matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kill {
    /// The **hard** phase: `CLOCK_THREAD_CPUTIME_ID` passed **B+G** and the
    /// epoch counter delivered the kill without unwinding. The grace window
    /// had already been offered and the dispatch did not take it.
    ///
    /// There is deliberately no `Kill::Clean`: a clean kill is not something
    /// the engine does *to* the guest, it is the guest returning. It leaves no
    /// trap and no error, only a `grace` record in the store.
    CpuHard,
    /// The wall-clock backstop. Never a meter — purely a guard against a
    /// dispatch pinned off-CPU forever. Bypasses grace: a dispatch pinned
    /// off-CPU is not burning budget, so there is nothing to warn it about.
    Wall,
}

/// The CPU budget state the epoch callback consults. The kill is thread-clock
/// metered and epoch-delivered: epochs are wall-clock ticks, so they cannot
/// themselves express a CPU budget; they exist to give us a periodic,
/// guest-preempting callback in which the thread clock is the thing actually
/// compared.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meter {
    pub cpu0_ns: u64,
    pub wall0: Option<Instant>,
    /// **B**. Crossing it enters grace; it is not itself fatal.
    pub cpu_budget_ns: u64,
    /// **G**, `max(2 ms, 10% of B)`. `cpu_budget_ns + grace_ns` is the hard
    /// deadline, and it is the only deadline that traps.
    pub grace_ns: u64,
    /// Thread CPU burned by earlier slices of this same dispatch. B and B+G
    /// are per-dispatch, not per-slice.
    pub spent_ns: u64,
    pub wall_cap_ns: u64,
    /// How far to extend the epoch deadline at each check.
    pub check_ticks: u64,
}

impl Meter {
    /// Thread CPU this dispatch has burned in total, earlier slices included.
    #[inline]
    pub fn dispatch_cpu_ns(&self, now_ns: u64) -> u64 {
        self.spent_ns
            .saturating_add(now_ns.saturating_sub(self.cpu0_ns))
    }

    /// The hard deadline B+G, saturating.
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

/// Exactly one linear memory, maximum = the plan ceiling.
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
    /// `Option` only so the engine can move the box out for the duration of an
    /// import call — `HostBindings` methods take `&mut self` while the encoded
    /// payload borrows the scratch buffer that lives in this same struct.
    pub bindings: Option<Box<dyn HostBindings>>,
    /// Set immediately after instantiation; the import path needs it and a
    /// per-call `get_export("memory")` lookup would be pure waste.
    pub memory: Option<Memory>,

    /// The coarse clock, **frozen** for the slice. This is a security control,
    /// not a convenience: with no threads, no shared memory and no fine clock,
    /// the guest has no measurement primitive to build a Spectre-class gadget
    /// on.
    pub clock_ms: u64,

    /// Number of wasm frames currently on this thread's native stack, as
    /// reported by wasmtime's own call hook — not by our bookkeeping around the
    /// call. Snapshot eligibility has to be *checked* rather than assumed, and
    /// this is the thing that gets checked.
    pub wasm_depth: i32,
    /// High-water mark, kept so tests can prove the counter tracks reality in
    /// both directions and so a reentrancy violation would be visible.
    pub max_wasm_depth: i32,

    pub meter: Meter,
    pub kill: Option<Kill>,

    /// Grace. `Some(cpu_ns)` from the moment the dispatch crossed B and the
    /// host was told — the cancellation future completed and imports began
    /// answering `Cancelled`.
    ///
    /// It is set only once per slice and only when the notification actually
    /// reached the host, so a deferral on the *unsafe-to-trap* bit leaves it
    /// `None` and the next epoch tick tries again.
    pub grace_at_ns: Option<u64>,
    /// The runtime layer's *unsafe-to-trap* bit. Set around GC phases and
    /// scheduler queue mutations by the `unsafe_to_trap` import.
    ///
    /// Grace entry **defers** while it is set. The hard deadline **ignores**
    /// it — which is the whole of "a pinned bit buys exactly G, nothing more".
    pub unsafe_to_trap: bool,
    /// How many epoch ticks found the bit set and deferred grace entry.
    pub grace_deferrals: u64,
    /// The import that violated capability policy, for `TrapInfo::message` and
    /// for pre-init's deferral loop, which needs to know *which* import an
    /// impure static initializer touched.
    pub violation: Option<&'static str>,
    /// Message passed to `abort`.
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
            grace_at_ns: None,
            unsafe_to_trap: false,
            grace_deferrals: 0,
            violation: None,
            abort_msg: None,
            limits: Limits {
                max_memory_bytes,
                oom: false,
            },
            scratch: Vec::new(),
        }
    }

    /// Clear the per-dispatch scratch state, keeping the bindings and the
    /// allocation capacity.
    pub(crate) fn begin_slice(&mut self, clock_ms: u64, meter: Meter) {
        self.clock_ms = clock_ms;
        self.meter = meter;
        self.kill = None;
        self.grace_at_ns = None;
        // The bit is a property of the runtime layer's *current* critical
        // section, not of the instance. A slice that ended inside one ended
        // because it trapped, and the next slice starts outside it.
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

// ---------------------------------------------------------------------------
// Typed errors raised out of the engine's own code, so trap classification is
// exact rather than a match on wasmtime's message strings.
// ---------------------------------------------------------------------------

/// The **hard** kill, raised from the epoch deadline callback at B+G.
///
/// The message names both numbers on purpose: a tenant reading
/// `budget_exceeded` needs to see that a grace window existed and was not
/// taken, because that is the difference between "my code is slow" and "my
/// code cannot be interrupted".
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

/// The wall-clock backstop. Distinct from `BudgetExceeded` because it is not a
/// CPU-budget event.
#[derive(Debug, thiserror::Error)]
#[error("wall-clock backstop tripped after {wall_ns} ns")]
pub(crate) struct WallCapExceeded {
    pub wall_ns: u64,
}

/// Invariant 4. Either an import the module did not declare (which
/// normally cannot happen — the linker defines only [`wisp_abi::imports::IMPORTS`]
/// — but is checked twice on purpose) or one that the sealed init sandbox
/// forbids, which is the signal that a static initializer is impure.
#[derive(Debug, thiserror::Error)]
#[error("capability violation: import `{import}` is not reachable here (sealed init sandbox: {init_sandbox})")]
pub(crate) struct CapabilityViolation {
    pub import: &'static str,
    pub init_sandbox: bool,
}

/// The guest called `abort`.
#[derive(Debug, thiserror::Error)]
#[error("guest aborted: {message}")]
pub(crate) struct GuestAbort {
    pub message: String,
}

/// `wisp_alloc` could not satisfy a host->guest buffer handoff.
#[derive(Debug, thiserror::Error)]
#[error("wisp_alloc({size}) returned 0: guest heap exhausted")]
pub(crate) struct AllocFailed {
    pub size: usize,
}

/// The host must not call into an instance while a previous call into it has
/// not returned. Enforced here as well as in `wisp-host` because a violation
/// would corrupt the meter and the frozen clock, not merely the guest.
#[derive(Debug, thiserror::Error)]
#[error("reentrant call into an instance with {depth} wasm frames already on the stack")]
pub(crate) struct Reentrancy {
    pub depth: i32,
}
