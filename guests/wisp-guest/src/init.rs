//! Host preparation and deferred initialization units.
//! A unit announces its stable slice index before it runs. A refused unit can be
//! skipped on a later bounded preparation attempt. Discarding guest state does not
//! undo earlier allowed external effects. A successful attempt becomes captured
//! guest state; restoration does not rerun its initialization.
//! Rust fixtures supply their explicit unit list through `init_units!`.

use core::cell::{Cell, UnsafeCell};

use crate::abi;

/// One pre-initialization unit. `id` is its index in the app's list, and that
/// index is what the host records when the unit traps.
#[derive(Clone, Copy)]
pub struct InitUnit {
    pub name: &'static str,
    pub run: fn(),
}

/// A value whose initializer may or may not survive the sealed init sandbox.
///
/// If it is pure it runs at deploy time and lands in the template, costing
/// every instance nothing. If it touches a forbidden import it traps, the host
/// defers it, and it runs here on first touch instead.
pub struct LazyInit<T: 'static> {
    name: &'static str,
    slot: UnsafeCell<Option<T>>,
    init: fn() -> T,
    /// How many times the initializer has actually executed *in this
    /// instance*. §2.6 makes reuse-vs-fresh observable and this is the number
    /// that shows it: under `reuse` it stays at 1, under `fresh` it climbs.
    runs: Cell<u32>,
}

// Single logical thread (§4.3 V2 turns threads/atomics off at the engine).
unsafe impl<T> Sync for LazyInit<T> {}

impl<T: 'static> LazyInit<T> {
    pub const fn new(name: &'static str, init: fn() -> T) -> LazyInit<T> {
        LazyInit { name, slot: UnsafeCell::new(None), init, runs: Cell::new(0) }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Run the initializer now if it has not run. Returns true if it ran.
    pub fn force(&self) -> bool {
        let slot = unsafe { &mut *self.slot.get() };
        if slot.is_none() {
            let v = (self.init)();
            // Assign after running: if `init` traps we must leave no
            // half-built value behind for the replay to trip over.
            *slot = Some(v);
            self.runs.set(self.runs.get() + 1);
            return true;
        }
        false
    }

    pub fn get(&self) -> &T {
        self.force();
        unsafe { (*self.slot.get()).as_ref().unwrap_unchecked() }
    }

    /// True if the value is already present -- i.e. it either pre-ran into the
    /// template or has been touched in this instance.
    pub fn is_initialized(&self) -> bool {
        unsafe { (*self.slot.get()).is_some() }
    }

    /// How many times this initializer has run in this instance.
    pub fn run_count(&self) -> u32 {
        self.runs.get()
    }

    /// Wrap this cell as a pre-init unit.
    pub const fn unit(&'static self, run: fn()) -> InitUnit {
        InitUnit { name: self.name, run }
    }
}

impl<T: 'static> core::ops::Deref for LazyInit<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.get()
    }
}

/// Declare a value whose initializer may be deferred to first touch (§4.4).
///
/// ```ignore
/// deferred_lazy! {
///     /// Pure: pre-runs at deploy time and lands in the template.
///     static ROUTES: Vec<&'static str> = { vec!["/quote", "/memo"] };
/// }
/// deferred_lazy! {
///     /// Impure: reads the clock, so it TRAPS in the sealed init sandbox,
///     /// the host defers it, and it runs per instance on first touch.
///     static STARTED_AT: u64 = { wisp_guest::Clock::now_ms() };
/// }
/// ```
#[macro_export]
macro_rules! deferred_lazy {
    ($( $(#[$m:meta])* $vis:vis static $name:ident : $ty:ty = $init:block $(;)? )*) => {$(
        $(#[$m])*
        $vis static $name: $crate::init::LazyInit<$ty> =
            $crate::init::LazyInit::new(stringify!($name), || $init);
    )*};
}

/// Collect [`deferred_lazy!`] statics into the ordered unit list the pre-init
/// protocol indexes by. Order is the contract: the host records unit *ids*.
#[macro_export]
macro_rules! init_units {
    ($($name:ident),* $(,)?) => {
        &[$( $crate::init::InitUnit { name: stringify!($name), run: || { $name.force(); } } ),*]
    };
}

/// Runs units in slice order with stable positional IDs, skipping units the host deferred.
pub fn run_init_units(units: &[InitUnit]) {
    for (id, u) in units.iter().enumerate() {
        // Announce BEFORE running: if `run` traps, this id is the only record
        // of which unit was responsible.
        unsafe { abi::init_unit_begin(id as i32) };
        if unsafe { abi::init_unit_deferred(id as i32) } != 0 {
            // The host already learned this one is impure. Skip it; it runs
            // lazily, per instance, on first touch.
            continue;
        }
        (u.run)();
    }
}

/// Reads the current preparation string-hash seed and retains it in guest state.
/// Calls after initialization use that captured value. The seed can be present in
/// templates and snapshots; do not put it in responses or logs.
pub fn hash_seed() -> [u8; 16] {
    let mut seed = [0u8; 16];
    unsafe { abi::init_hash_seed(seed.as_mut_ptr() as i32, seed.len() as i32) };
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    static PURE: LazyInit<u32> = LazyInit::new("PURE", || 41 + 1);

    #[test]
    fn lazy_runs_once_and_counts() {
        assert!(!PURE.is_initialized());
        assert_eq!(*PURE.get(), 42);
        assert!(PURE.is_initialized());
        assert_eq!(PURE.run_count(), 1);
        assert_eq!(*PURE.get(), 42);
        assert_eq!(PURE.run_count(), 1, "reuse amortizes a deferred initializer (§2.6)");
    }

    /// The unit list is index-addressed because the host records unit *ids*
    /// when one traps. Reordering it silently re-points the host's deferral
    /// record, so this asserts the macro preserves order.
    #[test]
    fn unit_ids_are_declaration_order() {
        static A: LazyInit<u8> = LazyInit::new("A", || 1);
        static B: LazyInit<u8> = LazyInit::new("B", || 2);
        let units: &[InitUnit] = init_units![A, B];
        assert_eq!(units[0].name, "A");
        assert_eq!(units[1].name, "B");
    }
}
