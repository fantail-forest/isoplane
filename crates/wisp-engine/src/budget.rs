//! CPU execution budgets and dispatch measurements.

use wisp_abi::status::PollStatus;

use crate::Completion;

/// CPU allowance and wall-clock backstop for a dispatch.
#[derive(Debug, Clone, Copy)]
pub struct CpuBudget {
    pub cpu_ms: u32,
    /// Wall-clock backstop against a dispatch pinned off-CPU.
    pub wall_ms_cap: u32,
}

impl CpuBudget {
    pub fn http() -> Self {
        Self {
            cpu_ms: 50,
            wall_ms_cap: 30_000,
        }
    }
    pub fn batch() -> Self {
        Self {
            cpu_ms: 300_000,
            wall_ms_cap: 600_000,
        }
    }
}

/// The result of one synchronous call into the guest.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub status: PollStatus,
    pub completion: Completion,
    /// Thread CPU nanoseconds measured during this execution slice.
    pub cpu_ns: u64,
    /// For observability only.
    pub wall_ns: u64,
    pub trap: Option<TrapInfo>,
}

#[derive(Debug, Clone)]
pub struct TrapInfo {
    pub kind: TrapKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// Execution stopped after exceeding its CPU budget.
    BudgetExceeded,
    /// Guest called `abort` or panicked.
    GuestPanic,
    /// Bounds check, bad indirect call, unreachable, etc.
    Sandbox,
    /// Linear memory could not grow within the configured memory ceiling.
    OutOfMemory,
    /// An import violated the active capability policy.
    CapabilityViolation,
    Other,
}

impl TrapKind {
    /// Diagnostic code reported for this trap category.
    pub fn tenant_code(self) -> &'static str {
        match self {
            TrapKind::BudgetExceeded => "budget_exceeded",
            TrapKind::GuestPanic => "guest_panic",
            TrapKind::Sandbox => "sandbox_violation",
            TrapKind::OutOfMemory => "out_of_memory",
            TrapKind::CapabilityViolation => "capability_violation",
            TrapKind::Other => "trap",
        }
    }
}
