//! CPU budgets, the two-phase kill, and metering.
//!
//! # The two phases
//!
//! ```text
//!   0                                B                 B+G
//!   |--------- normal execution ------|---- grace ------|
//!                                     ^                 ^
//!                                     |                 `- hard trap: no unwinding,
//!                                     |                    instance TOMBSTONED
//!                                     `- cancellation future completes,
//!                                        imports poisoned to `Cancelled`,
//!                                        `budget_remaining_ns` goes negative
//! ```
//!
//! `G = max(2 ms, 10% of B)`. A dispatch that unwinds and returns inside the
//! grace window receives a **clean kill**: `finally` runs, state is
//! consistent, the slot is reusable, and the overage CPU is recorded. One that
//! is still executing at `B+G` takes the **hard kill** — no `finally`, no
//! finalizers — which is the older behaviour, now scoped to exactly that case.

use wisp_abi::status::PollStatus;

use crate::Completion;

/// Default 50 ms for HTTP; opt-in to 5 min on the batch tier.
#[derive(Debug, Clone, Copy)]
pub struct CpuBudget {
    pub cpu_ms: u32,
    /// Wall-clock backstop against a dispatch pinned off-CPU.
    pub wall_ms_cap: u32,
    /// Thread CPU already burned by **earlier slices of the same dispatch**.
    ///
    /// B and B+G are properties of the *dispatch*, not of one Hot interval. A
    /// guest that parks inside grace and is woken by its own poisoned future
    /// must come back with the remainder of G, not with a fresh one —
    /// otherwise "a pinned bit buys exactly G, nothing more" would be false
    /// for any guest willing to park.
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

    /// **G**: `max(2 ms, 10% of B)`.
    ///
    /// The floor matters more than the fraction: 10% of a 5 ms budget is
    /// 500 µs, which is two and a half epoch ticks — not enough time for a
    /// managed runtime to notice, unwind and return, so grace would be a
    /// window that exists on paper and never in practice.
    pub fn grace_ns(self) -> u64 {
        (self.budget_ns() / 10).max(GRACE_FLOOR_NS)
    }

    /// The hard deadline, B+G. Reaching it is the only thing that still
    /// produces the un-unwound kill.
    pub fn hard_ns(self) -> u64 {
        self.budget_ns().saturating_add(self.grace_ns())
    }

    /// A budget for a later slice of a dispatch that has already burned
    /// `spent_ns` of thread CPU.
    pub fn after(self, spent_ns: u64) -> Self {
        Self { spent_ns, ..self }
    }
}

/// The floor on the grace window: `G = max(**2 ms**, 10% of B)`.
///
/// A constant rather than a knob — but one that only means anything
/// **relative to the epoch tick**, because the tick is how grace is
/// delivered. A floor of 2 ms buys ten checks at the default 200 µs
/// tick and none at all at a 5 ms one, and the failure at a coarse tick is
/// silent: every over-budget dispatch simply takes the hard trap, having been
/// offered a window the platform had no opportunity to deliver.
///
/// [`crate::EngineConfig`] is therefore validated against this at engine
/// construction rather than left to be discovered as a collapsed clean:hard
/// ratio. See `WispEngine::new`.
pub const GRACE_FLOOR_NS: u64 = 2_000_000;

/// How many epoch ticks must fit inside the **smallest** grace window the
/// platform can offer, for grace to be deliverable at all.
///
/// One tick would be arithmetically sufficient and operationally useless: the
/// check is Cranelift-inserted at back-edges and calls, so a guest between two
/// back-edges when the tick fires does not see it until the next one, and a
/// dispatch that got exactly one opportunity would take the hard trap on any
/// scheduling hiccup. Four is the smallest number that leaves the delivery
/// mechanism room to miss and recover inside the floor.
pub const MIN_GRACE_TICKS: u64 = 4;

/// Which phase of the two-phase kill ended a dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillPhase {
    /// The dispatch entered grace and **returned inside it**. `finally` ran,
    /// the instance is consistent, its slot is reusable, and the overage CPU
    /// is recorded.
    Clean,
    /// Still executing at B+G. Terminated without unwinding — no `finally`,
    /// no finalizers — and the instance is tombstoned.
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
    /// **The sole compute meter**: `CLOCK_THREAD_CPUTIME_ID`
    /// accumulated over this Hot interval. Exact under kernel preemption and
    /// immune to wall-clock noise — which is exactly why park time need never
    /// be metered.
    ///
    /// On a kill of either phase this includes the **overage**: the CPU burned
    /// between B and the moment the dispatch actually stopped. It is recorded.
    pub cpu_ns: u64,
    /// For observability only.
    pub wall_ns: u64,
    pub trap: Option<TrapInfo>,
    /// `Some` if this slice entered the grace window, saying which phase
    /// the dispatch ended in. `None` for a dispatch that stayed inside B.
    pub kill: Option<KillPhase>,
    /// Thread CPU burned past B, if grace was entered. Recorded, and broken out
    /// so the meter can report the overage separately from the budget.
    pub overage_ns: u64,
    /// How many times the epoch callback found the runtime layer's
    /// *unsafe-to-trap* bit set and deferred grace entry. A healthy
    /// number is small; a large one means a guest is pinning the bit, which
    /// buys it exactly G and no more.
    pub grace_deferrals: u64,
}

#[derive(Debug, Clone)]
pub struct TrapInfo {
    pub kind: TrapKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// The **hard** phase of the two-phase kill: the dispatch was still
    /// executing at B+G and was terminated **without unwinding** — no
    /// `finally`, no finalizers, no cleanup quantum. This costs nothing
    /// externally because every external resource is a host-owned handle the
    /// host reclaims on instance death; it costs everything *internally*,
    /// which is why the instance is tombstoned.
    ///
    /// A clean kill never produces a `TrapKind`: it is a return, not a trap.
    BudgetExceeded,
    /// Guest called `abort` or panicked.
    GuestPanic,
    /// Bounds check, bad indirect call, unreachable, etc.
    Sandbox,
    /// Linear memory could not grow within the configured memory ceiling.
    OutOfMemory,
    /// An undeclared import, or a forbidden one inside the init sandbox — the
    /// signal that a static initializer is impure and must be deferred to lazy
    /// per-instance execution.
    CapabilityViolation,
    Other,
}

impl TrapKind {
    /// What the tenant sees. Traps are never retried on the same memory.
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

    /// `G = max(2 ms, 10% of B)`.
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

    /// The epoch tick can deliver grace across the configured budget range.
    #[test]
    fn configured_cpu_budgets_get_deliverable_grace_windows() {
        let tick_ns = crate::EngineConfig::default().epoch_tick_us * 1_000;
        for budget_ms in [10, 30000, 300000] {
            let b = CpuBudget {
                cpu_ms: budget_ms,
                ..CpuBudget::http()
            };
            let g = b.grace_ns();
            assert!(g >= GRACE_FLOOR_NS, "G = {g} ns is below the grace floor");
            assert!(
                g / tick_ns >= MIN_GRACE_TICKS,
                "a {} ms budget gives G = {:.1} ms, only {} epoch tick(s) at \
                 {tick_ns} ns — a dispatch with this budget could reach B+G without the callback \
                 ever having had a chance to open the grace window, and the symptom would be a \
                 clean:hard ratio of zero rather than an error",
                b.cpu_ms,
                g as f64 / 1e6,
                g / tick_ns,
            );
            assert_eq!(b.hard_ns(), b.budget_ns() + g);
        }
    }

    /// **Negative control** for the check above: it must be able to say
    /// "undeliverable". Same arithmetic, a tick coarser than the floor.
    ///
    /// `Instance::slice` carries the same comparison as a `debug_assert`, at
    /// the point a dispatch actually needs the relationship to hold, so this
    /// pins the arithmetic that assertion rests on.
    ///
    /// The guard is at the dispatch and not at engine construction on purpose:
    /// a rig that builds engines *only* to measure the watchdog by
    /// differencing two tick rates uses a deliberately coarse arm and never
    /// activates a template, so it is a legitimate coarse-tick caller.
    /// Refusing at construction would have made the guard's first act to break
    /// a measurement it has nothing to do with.
    #[test]
    fn a_coarse_epoch_tick_makes_the_grace_floor_undeliverable() {
        let coarse_tick_ns = 5_000_000u64; // 5 ms
        assert!(
            GRACE_FLOOR_NS / coarse_tick_ns < MIN_GRACE_TICKS,
            "a 5 ms tick must not be able to deliver a 2 ms grace floor; if this passes, the \
             engine's configuration check is guarding nothing"
        );
    }

    /// **Negative control**. The floor must be a floor, not a constant:
    /// if `grace_ns` ever returned a flat 2 ms, this fails.
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
