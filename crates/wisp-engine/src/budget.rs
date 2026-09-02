//! CPU budgets and the two-phase kill.
//!
//! ```text
//!   0                                B                 B+G
//!   |--------- normal execution ------|---- grace ------|
//!                                     ^                 ^
//!                                     |                 `- hard trap: no unwinding,
//!                                     |                    instance tombstoned
//!                                     `- cancellation future completes,
//!                                        imports poisoned to `Cancelled`,
//!                                        `budget_remaining_ns` goes negative
//! ```
//!
//! `G = max(2 ms, 10% of B)`. A dispatch that returns inside the grace window
//! takes a clean kill: the instance is consistent and its overage is recorded.
//! One still executing at B+G takes the hard kill.

use anyhow::bail;
use wisp_abi::status::PollStatus;

use crate::Completion;

#[derive(Debug, Clone, Copy)]
pub struct CpuBudget {
    pub cpu_ms: u32,
    /// Wall-clock backstop against a dispatch pinned off-CPU.
    pub wall_ms_cap: u32,
    /// Thread CPU burned by earlier slices of the same dispatch, so a guest
    /// that parks inside grace comes back with the remainder of G rather than
    /// a fresh one.
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

    pub fn budget_ns(self) -> u64 {
        (self.cpu_ms as u64).saturating_mul(1_000_000)
    }

    /// G. The floor matters more than the fraction: a tenth of a small budget
    /// is a couple of epoch ticks, not enough for a managed runtime to notice,
    /// unwind and return.
    pub fn grace_ns(self) -> u64 {
        (self.budget_ns() / 10).max(GRACE_FLOOR_NS)
    }

    /// The hard deadline, B+G.
    pub fn hard_ns(self) -> u64 {
        self.budget_ns().saturating_add(self.grace_ns())
    }

    /// The budget for a later slice of a dispatch that has already burned
    /// `spent_ns`.
    pub fn after(self, spent_ns: u64) -> Self {
        Self { spent_ns, ..self }
    }
}

/// The floor on the grace window.
pub const GRACE_FLOOR_NS: u64 = 2_000_000;

/// How many epoch ticks must fit inside the smallest grace window for grace
/// to be deliverable. One would be arithmetically sufficient and useless: the
/// check is emitted at back-edges and calls, so a guest between two back-edges
/// when the tick fires does not see it until the next one.
pub const MIN_GRACE_TICKS: u64 = 4;

/// Refuses an epoch tick too coarse to deliver the grace floor. The failure
/// this guards is silent: every over-budget dispatch takes the hard trap
/// having been offered a window no tick could open.
pub(crate) fn check_grace_deliverable(epoch_tick_us: u64) -> crate::Result<()> {
    let tick_ns = epoch_tick_us.max(1).saturating_mul(1_000);
    let ticks_in_floor = GRACE_FLOOR_NS / tick_ns;
    if ticks_in_floor < MIN_GRACE_TICKS {
        bail!(
            "epoch_tick_us = {epoch_tick_us} fits only {ticks_in_floor} tick(s) inside the \
             {} ms grace floor, fewer than the {MIN_GRACE_TICKS} needed to deliver grace; every \
             budget kill on this engine would be a hard kill",
            GRACE_FLOOR_NS / 1_000_000
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillPhase {
    /// Entered grace and returned inside it.
    Clean,
    /// Still executing at B+G; terminated without unwinding and tombstoned.
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
    /// Thread CPU over this hot interval, overage included. The compute meter.
    pub cpu_ns: u64,
    /// For observability only.
    pub wall_ns: u64,
    pub trap: Option<TrapInfo>,
    /// `Some` when this slice ended a dispatch that entered grace.
    pub kill: Option<KillPhase>,
    /// Thread CPU burned past B, broken out so the meter can report it apart
    /// from the budget.
    pub overage_ns: u64,
    /// Epoch ticks that found the unsafe-to-trap bit set and deferred grace
    /// entry.
    pub grace_deferrals: u64,
}

#[derive(Debug, Clone)]
pub struct TrapInfo {
    pub kind: TrapKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// The hard kill. A clean kill is a return, not a trap.
    BudgetExceeded,
    /// Guest called `abort` or panicked.
    GuestPanic,
    /// Bounds check, bad indirect call, unreachable.
    Sandbox,
    /// Linear memory could not grow within the configured memory ceiling.
    OutOfMemory,
    /// An undeclared import, or a forbidden one inside the init sandbox.
    CapabilityViolation,
    /// A call into an instance that a previous kill or trap tombstoned.
    Tombstoned,
    Other,
}

impl TrapKind {
    pub fn tenant_code(self) -> &'static str {
        match self {
            TrapKind::BudgetExceeded => "budget_exceeded",
            TrapKind::GuestPanic => "guest_panic",
            TrapKind::Sandbox => "sandbox_violation",
            TrapKind::OutOfMemory => "out_of_memory",
            TrapKind::CapabilityViolation => "capability_violation",
            TrapKind::Tombstoned => "tombstoned",
            TrapKind::Other => "trap",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_is_the_larger_of_two_milliseconds_and_a_tenth_of_the_budget() {
        assert_eq!(
            CpuBudget::http().grace_ns(),
            5_000_000,
            "a tenth of the 50 ms default dominates the floor"
        );
        assert_eq!(
            CpuBudget::http().hard_ns(),
            55_000_000,
            "the hard deadline is B plus G"
        );
        let small = CpuBudget {
            cpu_ms: 5,
            ..CpuBudget::http()
        };
        assert_eq!(
            small.grace_ns(),
            2_000_000,
            "a tenth of 5 ms is below the floor, so the floor applies"
        );
        let large = CpuBudget {
            cpu_ms: 300_000,
            ..CpuBudget::http()
        };
        assert_eq!(
            large.grace_ns(),
            30_000_000_000,
            "G scales with B above the floor rather than sitting at it"
        );
    }

    #[test]
    fn configured_cpu_budgets_get_deliverable_grace_windows() {
        let tick_ns = crate::EngineConfig::default().epoch_tick_us * 1_000;
        for budget_ms in [10, 30000, 300000] {
            let b = CpuBudget {
                cpu_ms: budget_ms,
                ..CpuBudget::http()
            };
            let g = b.grace_ns();
            assert!(
                g / tick_ns >= MIN_GRACE_TICKS,
                "a {} ms budget gives G = {g} ns, only {} epoch tick(s) at {tick_ns} \
                 ns; a dispatch with this budget could reach B+G before grace could be opened",
                b.cpu_ms,
                g / tick_ns,
            );
        }
    }

    #[test]
    fn a_coarse_epoch_tick_is_refused() {
        assert!(
            check_grace_deliverable(5_000).is_err(),
            "a 5 ms tick cannot deliver a 2 ms grace floor and must be refused"
        );
        assert!(
            check_grace_deliverable(500).is_ok(),
            "a 500 us tick fits exactly the minimum ticks and is accepted"
        );
    }
}
