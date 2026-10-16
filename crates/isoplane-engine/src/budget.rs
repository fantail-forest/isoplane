//! Worker occupancy budgets and the two-phase kill.
//!
//! ```text
//!   0                                B                 B+G
//!   |--------- normal execution ------|---- grace ------|
//!                                     ^                 ^
//!                                     |                 `- hard trap: no unwinding,
//!                                     |                    instance discarded
//!                                     `- cancellation requested: from the
//!                                        guest's next slice its imports
//!                                        answer `Cancelled` and its
//!                                        cancellation future completes
//! ```
//!
//! `G = max(2 ms, 10% of B)`. A dispatch that returns inside the grace window
//! takes a clean kill: the instance is consistent and its overage is recorded.
//! A check observing B+G or more takes the hard kill. Checks are best effort;
//! scheduler delays can skip grace or delay termination.

use anyhow::bail;
use isoplane_abi::status::PollStatus;

use crate::Completion;

#[derive(Debug, Clone, Copy)]
pub struct OccupancyBudget {
    pub occupancy_ms: u32,
    /// Independent elapsed-time cap for one invocation, including debugger sessions.
    pub wall_ms_cap: u32,
    /// Worker occupancy burned by earlier slices of the same dispatch, so a guest
    /// that parks inside grace comes back with the remainder of G rather than
    /// a fresh one.
    pub spent_ns: u64,
}

impl OccupancyBudget {
    pub fn http() -> Self {
        Self {
            occupancy_ms: 50,
            wall_ms_cap: 30_000,
            spent_ns: 0,
        }
    }

    pub fn budget_ns(self) -> u64 {
        (self.occupancy_ms as u64).saturating_mul(1_000_000)
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

/// Minimum nominal epoch ticks within the smallest grace window.
/// Scheduling delays can still prevent grace delivery.
pub const MIN_GRACE_TICKS: u64 = 4;

/// Refuses an epoch interval too coarse for the configured grace policy.
pub(crate) fn check_grace_deliverable(epoch_tick_us: u64) -> crate::Result<()> {
    let tick_ns = epoch_tick_us.max(1).saturating_mul(1_000);
    let ticks_in_floor = GRACE_FLOOR_NS / tick_ns;
    if ticks_in_floor < MIN_GRACE_TICKS {
        bail!(
            "epoch_tick_us = {epoch_tick_us} fits only {ticks_in_floor} tick(s) inside the \
             {} ms grace floor, fewer than the {MIN_GRACE_TICKS} required by the grace policy",
            GRACE_FLOOR_NS / 1_000_000
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillPhase {
    /// Entered grace and returned before a hard-kill check.
    Clean,
    /// A check observed B+G or more; terminated without unwinding and discarded.
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

/// Whether a returned occupancy value accounts for the entire invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccupancyMeasurement {
    /// Both clock readings succeeded.
    Complete,
    /// No guest invocation was attempted.
    NotExecuted,
    /// The occupancy value is only a known floor; the end reading was unavailable.
    Incomplete,
}

/// The existing execution bound that ended this invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionLimit {
    /// Worker occupancy exhausted its budget, including any grace.
    Occupancy,
    /// The invocation's wall-clock backstop fired.
    Wall,
}

/// The result of one synchronous call into the guest.
#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub status: PollStatus,
    pub completion: Completion,
    /// Worker occupancy over this hot interval, overage included. The compute meter.
    pub occupancy_ns: u64,
    /// Completeness of the occupancy floor above.
    pub occupancy_measurement: OccupancyMeasurement,
    /// Typed discriminator for a budget or wall backstop outcome.
    pub execution_limit: Option<ExecutionLimit>,
    /// For observability only.
    pub wall_ns: u64,
    pub trap: Option<TrapInfo>,
    /// `Some` when this slice ended a dispatch that entered grace.
    pub kill: Option<KillPhase>,
    /// Worker occupancy burned past B, broken out so the meter can report it apart
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
    /// A call into an instance a previous kill or trap discarded.
    Discarded,
    /// A host occupancy clock read failed, so execution could not enforce its budget.
    OccupancyClockUnavailable,
    /// Storage beneath the instance's mapped template failed a read: the
    /// platform's fault, not the guest's.
    StorageFault,
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
            TrapKind::Discarded => "discarded",
            TrapKind::OccupancyClockUnavailable => "occupancy_clock_unavailable",
            TrapKind::StorageFault => "storage_fault",
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
            OccupancyBudget::http().grace_ns(),
            5_000_000,
            "a tenth of the 50 ms default dominates the floor"
        );
        assert_eq!(
            OccupancyBudget::http().hard_ns(),
            55_000_000,
            "the hard deadline is B plus G"
        );
        let small = OccupancyBudget {
            occupancy_ms: 5,
            ..OccupancyBudget::http()
        };
        assert_eq!(
            small.grace_ns(),
            2_000_000,
            "a tenth of 5 ms is below the floor, so the floor applies"
        );
        let large = OccupancyBudget {
            occupancy_ms: 300_000,
            ..OccupancyBudget::http()
        };
        assert_eq!(
            large.grace_ns(),
            30_000_000_000,
            "G scales with B above the floor rather than sitting at it"
        );
    }

    #[test]
    fn configured_occupancy_budgets_get_deliverable_grace_windows() {
        let tick_ns = crate::EngineConfig::default().epoch_tick_us * 1_000;
        for budget_ms in [100, 30000, 300000] {
            let b = OccupancyBudget {
                occupancy_ms: budget_ms,
                ..OccupancyBudget::http()
            };
            let g = b.grace_ns();
            assert!(
                g / tick_ns >= MIN_GRACE_TICKS,
                "a {} ms budget gives G = {g} ns, only {} epoch tick(s) at {tick_ns} \
                 ns; a dispatch with this budget could reach B+G before grace could be opened",
                b.occupancy_ms,
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
