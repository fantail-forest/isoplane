//! Dispatch CPU budgets, cooperative grace, hard termination and measurements.

use wisp_abi::status::PollStatus;

use crate::Completion;

/// CPU allowance and wall-clock backstop for a dispatch.
#[derive(Debug, Clone, Copy)]
pub struct CpuBudget {
    pub cpu_ms: u32,
    /// Wall-clock backstop against a dispatch pinned off-CPU.
    pub wall_ms_cap: u32,
    /// CPU nanoseconds consumed by earlier slices of this dispatch.
    pub spent_ns: u64,
}

impl CpuBudget {
    pub fn http() -> Self {
        Self {
            cpu_ms: 50,
            wall_ms_cap: 30_000,
            spent_ns: 0,
        }
    }
    pub fn batch() -> Self {
        Self {
            cpu_ms: 300_000,
            wall_ms_cap: 600_000,
            spent_ns: 0,
        }
    }

    /// The budget B, in nanoseconds.
    pub fn budget_ns(self) -> u64 {
        (self.cpu_ms as u64).saturating_mul(1_000_000)
    }

    /// Grace allowance in nanoseconds: the larger of two milliseconds and ten percent of the budget.
    pub fn grace_ns(self) -> u64 {
        const FLOOR_NS: u64 = 2_000_000;
        (self.budget_ns() / 10).max(FLOOR_NS)
    }

    /// The hard deadline, B+G. Reaching it is the only thing that still
    /// produces v1.1's un-unwound kill.
    pub fn hard_ns(self) -> u64 {
        self.budget_ns().saturating_add(self.grace_ns())
    }

    /// A budget for a later slice of a dispatch that has already burned
    /// `spent_ns` of thread CPU.
    pub fn after(self, spent_ns: u64) -> Self {
        Self { spent_ns, ..self }
    }
}

/// How execution ended after entering the CPU grace allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillPhase {
    /// The guest returned after entering the grace allowance.
    Clean,
    /// Execution reached the hard deadline and trapped without guest unwinding.
    Hard,
}

impl KillPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            KillPhase::Clean => "clean",
            KillPhase::Hard => "hard",
        }
    }
}

/// The result of one synchronous call into the guest.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub status: PollStatus,
    pub completion: Completion,
    /// Thread CPU nanoseconds measured during this slice, including any budget overage.
    pub cpu_ns: u64,
    /// For observability only.
    pub wall_ns: u64,
    pub trap: Option<TrapInfo>,
    /// Termination phase when this slice entered grace; otherwise `None`.
    pub kill: Option<KillPhase>,
    /// Thread CPU burned past B, if grace was entered. Recorded, and broken out
    /// so the meter can report the overage separately from the budget.
    pub overage_ns: u64,
    /// Epoch callbacks that deferred grace entry while the runtime marked itself unsafe to trap.
    pub grace_deferrals: u64,
}

#[derive(Debug, Clone)]
pub struct TrapInfo {
    pub kind: TrapKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// Execution trapped at the hard CPU deadline rather than returning during grace.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_is_the_larger_of_two_milliseconds_and_a_tenth_of_the_budget() {
        // 50 ms HTTP default: 10% dominates.
        assert_eq!(CpuBudget::http().grace_ns(), 5_000_000);
        assert_eq!(CpuBudget::http().hard_ns(), 55_000_000);
        // 5 min dispatch: still 10%.
        assert_eq!(CpuBudget::batch().grace_ns(), 30_000_000_000);
        // Small budgets take the floor, which is the case the floor exists for.
        let small = CpuBudget {
            cpu_ms: 5,
            ..CpuBudget::http()
        };
        assert_eq!(
            small.grace_ns(),
            2_000_000,
            "10% of 5 ms is below the floor"
        );
        assert_eq!(small.hard_ns(), 7_000_000);
        let one = CpuBudget {
            cpu_ms: 1,
            ..CpuBudget::http()
        };
        assert_eq!(one.grace_ns(), 2_000_000);
    }

    #[test]
    fn grace_is_not_a_fixed_two_milliseconds() {
        assert_ne!(
            CpuBudget::http().grace_ns(),
            2_000_000,
            "G must scale with B above 20 ms, not sit at the floor forever"
        );
        assert!(CpuBudget::batch().grace_ns() > CpuBudget::http().grace_ns());
    }
}
