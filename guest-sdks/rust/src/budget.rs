//! Cooperating with the two-phase kill.
//!
//! At its CPU budget **B** a dispatch enters a **grace** window of
//! `G = max(2 ms, 10% of B)`, and at B+G it is hard-killed:
//!
//! | the guest… | outcome | its instance |
//! |---|---|---|
//! | returns inside G | **clean kill** | consistent, `Drop` ran, **slot reused** |
//! | is still executing at B+G | **hard kill** | trapped mid-frame, **discarded** |
//!
//! A guest learns that its budget ran out only at its next slice: every host
//! future it awaits from then on resolves to `Cancelled`, as does
//! [`crate::Ctx::cancelled`]. Guest-visible cancellation stays fixed within a
//! slice, so a guest that wants a clean kill does long work in chunks and awaits between
//! them. SECURITY: there is deliberately no way to read the remaining budget.
//! A value that falls while the guest runs is a clock, and a clock finer than
//! the frozen slice clock is what Spectre-class side channels need.
//!
//! Guest modules build with `panic = "abort"` (see `guests/Cargo.toml`), so
//! the unwind is an ordinary early `return` and the `finally` semantics are
//! `Drop`.

use crate::abi;

/// RAII guard for the *unsafe-to-trap* bit.
///
/// Hold one across a region that must not be interrupted **cleanly** — a GC
/// phase, a scheduler queue mutation, any window where delivering `Cancelled`
/// into half-mutated state would produce exactly the internal inconsistency
/// a hard kill exists to contain. While it is held, grace *entry* defers.
///
/// What it does **not** do, and must not be mistaken for: it does not defer
/// the hard deadline. The engine checks B+G first and ignores the bit there,
/// so a pinned bit buys exactly G and nothing more. A guest that holds one
/// forever is hard-killed at B+G exactly like one that never heard of it, and
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

    /// The guard must clear the bit on the way out, including on an early
    /// return, or a guest would pin it by accident and discover the fact only
    /// as a hard kill.
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
}
