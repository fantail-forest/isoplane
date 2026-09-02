//! Host preparation and deferred initialization units.
//! A unit announces its stable slice index before it runs. A refused unit can be
//! skipped on a later bounded preparation attempt. Discarding guest state does not
//! undo earlier allowed external effects. A successful attempt becomes captured
//! guest state; restoration does not rerun its initialization.
//! Rust fixtures supply their explicit unit list through `init_units!`.

use core::cell::{Cell, UnsafeCell};

use crate::abi;

/// One pre-initialization unit. Its id is its index in the app's list, and
/// that id is what the host records when the unit traps.
#[derive(Clone, Copy)]
pub struct InitUnit {
    pub name: &'static str,
    pub run: fn(),
}

/// "No unit in scope". `wisp_control::preinit::NO_UNIT` is `u32::MAX`; this is
/// the same bit pattern as an `i32`, because the import takes `i32`.
///
/// Deliberately **not** 0: unit 0 is the first static initializer in every
/// module, and reserving it would make the most common trap unattributable.
pub const NO_UNIT: i32 = -1;

/// The id the host will see for the unit at `index`.
pub const fn unit_id(index: usize) -> i32 {
    index as i32
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
    /// instance*. This is the number that makes reuse-vs-fresh observable:
    /// under `reuse` it stays at 1, under `fresh` it climbs.
    runs: Cell<u32>,
}

// Single logical thread: the engine has no threads and no atomics, and the
// validator rejects a module that declares them.
unsafe impl<T> Sync for LazyInit<T> {}

impl<T: 'static> LazyInit<T> {
    pub const fn new(name: &'static str, init: fn() -> T) -> LazyInit<T> {
        LazyInit {
            name,
            slot: UnsafeCell::new(None),
            init,
            runs: Cell::new(0),
        }
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
        InitUnit {
            name: self.name,
            run,
        }
    }
}

impl<T: 'static> core::ops::Deref for LazyInit<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.get()
    }
}

/// Declare a value whose initializer may be deferred to first touch.
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
    // `abort` is init-visible precisely so that a failed pre-init reports
    // usefully.
    #[cfg(target_arch = "wasm32")]
    crate::install_panic_hook();
    for (index, u) in units.iter().enumerate() {
        let id = unit_id(index);
        // Announce BEFORE running: if `run` traps, this id is the only record
        // of which unit was responsible.
        unsafe { abi::init_unit_begin(id) };
        if unsafe { abi::init_unit_deferred(id) } != 0 {
            // The host already learned this one is impure, and clears its own
            // current-unit when it answers 1. Skip; it runs lazily, per
            // instance, on first touch.
            continue;
        }
        (u.run)();
        // Leave unit scope. Nothing between units can touch a forbidden
        // import today, but if that ever changes the host must fail the
        // deploy loudly rather than blame the unit that just succeeded --
        // which would defer a pure unit forever and never converge.
        unsafe { abi::init_unit_begin(NO_UNIT) };
    }
    // Init-only imports stay DECLARED in the template -- wizer does not strip
    // the import section -- but the host binds them to traps at runtime as a
    // tripwire. From here on the SDK must not call any of them.
    state().init_complete = true;
}

struct InitState {
    seed: [u8; 16],
    seed_read: bool,
    config: Vec<u8>,
    config_read: bool,
    init_complete: bool,
}

struct StateCell(core::cell::UnsafeCell<InitState>);
unsafe impl Sync for StateCell {}

static STATE: StateCell = StateCell(core::cell::UnsafeCell::new(InitState {
    seed: [0u8; 16],
    seed_read: false,
    config: Vec::new(),
    config_read: false,
    init_complete: false,
}));

#[allow(clippy::mut_from_ref)]
fn state() -> &'static mut InitState {
    unsafe { &mut *STATE.0.get() }
}

/// Reads the current preparation string-hash seed and retains it in guest state.
/// Calls after initialization use that captured value. The seed can be present in
/// templates and snapshots; do not put it in responses or logs.
pub fn hash_seed() -> [u8; 16] {
    let st = state();
    if st.seed_read || st.init_complete {
        return st.seed;
    }
    // Exactly 16 bytes: any other length traps host-side.
    unsafe { abi::init_hash_seed(st.seed.as_mut_ptr() as i32, 16) };
    st.seed_read = true;
    st.seed
}

/// True once the seed has been captured. Safe to report; the seed itself is
/// not.
pub fn hash_seed_is_bound() -> bool {
    let st = state();
    st.seed_read && st.seed != [0u8; 16]
}

// ---------------------------------------------------------------------------
// Init-visible config
// ---------------------------------------------------------------------------

/// Reads initialization configuration for this preparation and caches it in guest state.
/// Later calls use that captured copy; configuration can contain sensitive values.
pub fn config_blob() -> Vec<u8> {
    let st = state();
    if st.config_read || st.init_complete {
        return st.config.clone();
    }
    // Zero-length probe for the size, then one call to fill.
    let full = abi::read_sized_out(abi::init_config_get, &mut []);
    if full > 0 {
        let mut buf = vec![0u8; full as usize];
        let got = abi::read_sized_out(abi::init_config_get, &mut buf);
        // The blob cannot change inside a sealed sandbox, but truncate rather
        // than trust the second answer.
        buf.truncate((got.max(0) as usize).min(full as usize));
        st.config = buf;
    }
    st.config_read = true;
    st.config.clone()
}

/// Test support (non-wasm builds only): forget the captured seed, config and
/// the "init is over" flag, so a test binary can drive pre-init more than
/// once. A real instance gets this for free -- the state lives in linear
/// memory, so a reset to template zeroes it.
#[cfg(not(target_arch = "wasm32"))]
pub fn reset_for_test() {
    let st = state();
    st.seed = [0u8; 16];
    st.seed_read = false;
    st.config = Vec::new();
    st.config_read = false;
    st.init_complete = false;
}

/// True if the deploy supplied any init-visible config.
pub fn config_is_bound() -> bool {
    let st = state();
    (st.config_read || st.init_complete) && !st.config.is_empty()
}

/// The init-visible vars as `(name, value)` pairs.
///
/// CONTRACT GAP: the blob's encoding is not declared in `wisp-abi`. The
/// control plane emits `serde_json::to_vec(&Vec<(String, String)>)`
/// (`wisp_control::pipeline::encode_init_vars`), so that is what this decodes.
/// Two sides guessing the same thing is not the same as a contract.
#[cfg(feature = "json")]
pub fn config_vars() -> Vec<(String, String)> {
    let blob = config_blob();
    if blob.is_empty() {
        return Vec::new();
    }
    serde_json::from_slice(&blob).unwrap_or_default()
}

/// One init-visible var by name.
#[cfg(feature = "json")]
pub fn config_get(name: &str) -> Option<String> {
    config_vars()
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
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
        assert_eq!(
            PURE.run_count(),
            1,
            "reuse amortizes a deferred initializer"
        );
    }

    /// The unit list is index-addressed because the host records unit *ids*
    /// when one traps. Reordering it silently re-points the host's deferral
    /// record, so this asserts the macro preserves order -- and that id 0 is a
    /// real unit, distinct from the [`NO_UNIT`] sentinel.
    #[test]
    fn unit_ids_are_stable_declaration_order() {
        static A: LazyInit<u8> = LazyInit::new("A", || 1);
        static B: LazyInit<u8> = LazyInit::new("B", || 2);
        let units: &[InitUnit] = init_units![A, B];
        assert_eq!(units[0].name, "A");
        assert_eq!(units[1].name, "B");
        assert_eq!(unit_id(0), 0);
        assert_eq!(unit_id(1), 1);
        // 0 is a REAL unit id; the sentinel is -1 (u32::MAX host-side).
        assert_ne!(unit_id(0), NO_UNIT);
        assert_eq!(NO_UNIT, u32::MAX as i32);
        // The same const slice every pass: that is the whole of "ids are
        // stable across runs".
        let again: &[InitUnit] = init_units![A, B];
        assert_eq!(units.len(), again.len());
        for (x, y) in units.iter().zip(again) {
            assert_eq!(x.name, y.name);
        }
    }
}
