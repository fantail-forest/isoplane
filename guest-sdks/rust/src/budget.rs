//! Cooperating with the budget.
//!
//! A guest learns that its budget ran out only at its next slice: every host
//! future it awaits from then on resolves to `Cancelled`, as does
//! [`crate::Ctx::cancelled`]. Cancellation stays fixed within a slice, so a
//! guest that wants to finish does long work in chunks and awaits between
//! them. A guest that returns after cancellation has run its `Drop`s and its
//! instance can be used again; one still executing when its grace ends is
//! trapped where it stands and its instance is discarded.
//!
//! SECURITY: there is deliberately no way to read the remaining budget.
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
/// a kill exists to contain. While it is held, grace *entry* defers.
///
/// It does not defer the end of the grace. A guest that holds one forever is
/// trapped there exactly like one that never pinned the bit.
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
    /// as a kill.
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
