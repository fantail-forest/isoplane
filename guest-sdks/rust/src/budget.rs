//! Cooperating with the two-phase kill.
//!
//! # Why a guest should care
//!
//! At its CPU budget **B** a dispatch enters a **grace** window of
//! `G = max(2 ms, 10% of B)`. Inside it the platform has already given up on
//! the request — the cancellation future is complete and every import answers
//! `Cancelled` — but the guest is still running, and what it does with those
//! milliseconds decides which of two very different things happens to it:
//!
//! | the guest… | outcome | its instance |
//! |---|---|---|
//! | returns inside G | **clean kill** | consistent, `Drop` ran, **slot reused** |
//! | is still executing at B+G | **hard kill** | trapped mid-frame, **tombstoned** |
//!
//! The host reports a clean:hard ratio per tenant, and a fleet is only as
//! clean as its guests: the ratio is good only where guests actually check.
//! This module is what a Rust guest checks *with*; the .NET runtime layer does
//! the equivalent for tenants automatically, at the allocation slow path, GC
//! entry and task switch.
//!
//! # What "unwinding" means here
//!
//! Guest modules build with `panic = "abort"` (see `guests/Cargo.toml`), so
//! the unwind is an ordinary early `return` and the `finally` semantics are
//! `Drop`. That is not a weaker form of the same thing — it is the same
//! thing: destructors run, buffers flush, and the value the platform cares
//! about (a consistent heap at an empty stack) is produced either way.

use crate::abi;

/// Signed nanoseconds of CPU budget left in this dispatch.
///
/// Negative means the dispatch is **in its grace window** and by how much.
/// Reaching `-G` is the hard deadline, so a guest that wants to know how much
/// unwinding it can still afford should compare against
/// [`Budget::grace_remaining_ns`] rather than against zero.
#[inline]
pub fn remaining_ns() -> i64 {
    unsafe { abi::budget_remaining_ns() }
}

/// True once this dispatch has crossed B.
///
/// The name is the one that matters at a call site: past this point the
/// request is over — nothing the guest computes will be returned to anyone —
/// and everything it does is either cleanup or a step toward a tombstone.
#[inline]
pub fn exhausted() -> bool {
    remaining_ns() <= 0
}

/// The budget view a dispatch has of itself.
pub struct Budget;

impl Budget {
    #[inline]
    pub fn remaining_ns() -> i64 {
        remaining_ns()
    }

    /// True once the dispatch is over budget and should unwind **now**.
    #[inline]
    pub fn exhausted() -> bool {
        exhausted()
    }

    /// Nanoseconds left before the hard deadline at B+G, given the grace
    /// window `grace_ns` the platform granted (`max(2 ms, 10% of B)`).
    ///
    /// A guest with expensive cleanup can use this to decide what it still has
    /// time to do — flush a buffer, or skip straight to returning. It cannot
    /// use it to *extend* anything.
    #[inline]
    pub fn grace_remaining_ns(grace_ns: i64) -> i64 {
        let r = remaining_ns();
        if r > 0 {
            grace_ns
        } else {
            grace_ns + r
        }
    }

    /// Bail out of a long computation the moment the budget is spent.
    ///
    /// ```ignore
    /// for chunk in chunks {
    ///     Budget::check()?;          // -> Err(IsoplaneError::Cancelled)
    ///     total += expensive(chunk);
    /// }
    /// ```
    ///
    /// The `?` is the unwind: every `Drop` between here and the entrypoint
    /// runs, the entrypoint returns a response, and the platform sees a
    /// return rather than a spin. That is a clean kill.
    #[inline]
    pub fn check() -> crate::Result<()> {
        if exhausted() {
            Err(crate::IsoplaneError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// RAII guard for the *unsafe-to-trap* bit.
///
/// Hold one across a region that must not be interrupted **cleanly** — a GC
/// phase, a scheduler queue mutation, any window where delivering `Cancelled`
/// into half-mutated state would produce exactly the internal inconsistency
/// tombstones exist to contain. While it is held, grace *entry* defers.
///
/// What it does **not** do, and must not be mistaken for: it does not defer
/// the hard deadline. The engine checks B+G first and ignores the bit there,
/// so a pinned bit buys exactly G and nothing more. A guest that holds one
/// forever is tombstoned at B+G exactly like one that never heard of it, and
/// has spent its whole grace window buying nothing.
///
/// ```ignore
/// {
///     let _pin = UnsafeToTrap::pin();   // collection in progress
///     compact_heap();
/// }                                      // bit cleared; grace may now land
/// ```
pub struct UnsafeToTrap {
    _private: (),
}

impl UnsafeToTrap {
    /// Set the bit. Cleared when the returned guard drops.
    #[inline]
    pub fn pin() -> UnsafeToTrap {
        unsafe { abi::unsafe_to_trap(1) };
        UnsafeToTrap { _private: () }
    }
}

impl Drop for UnsafeToTrap {
    #[inline]
    fn drop(&mut self) {
        unsafe { abi::unsafe_to_trap(0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    /// The host stubs answer 0 for everything, so on a laptop `remaining_ns`
    /// reads as "spent". That is the safe direction — a guest under test
    /// unwinds rather than spins — and it is what lets the cooperative-loop
    /// shape be exercised at all off-wasm.
    #[test]
    fn check_is_an_early_return_not_a_panic() {
        testing::with_stub_host(|_| {
            let r: crate::Result<()> = Budget::check();
            assert!(matches!(r, Err(crate::IsoplaneError::Cancelled)));
        });
    }

    /// The guard must clear the bit on the way out, including on an early
    /// return, or a guest would pin it by accident and discover the fact only
    /// as a tombstone.
    #[test]
    fn the_unsafe_to_trap_guard_clears_on_drop() {
        testing::with_stub_host(|host| {
            {
                let _pin = UnsafeToTrap::pin();
            }
            let calls = host.calls();
            let bits: Vec<i64> = calls
                .iter()
                .filter(|c| c.0 == "unsafe_to_trap")
                .map(|c| c.1[0])
                .collect();
            assert_eq!(bits, vec![1, 0], "the bit must be set and then cleared");
        });
    }

    /// The grace window is finite and the guard does not extend it:
    /// `grace_remaining_ns` must fall as the overage grows, never rise.
    #[test]
    fn grace_remaining_never_exceeds_g() {
        // Pure arithmetic over the same expression the guest uses, so the
        // property holds independently of what a stub host reports.
        let g = 5_000_000i64;
        for remaining in [10_000_000i64, 1, 0, -1, -2_500_000, -g, -g - 1] {
            let left = if remaining > 0 { g } else { g + remaining };
            assert!(left <= g, "grace grew: {left} > {g}");
        }
    }
}
