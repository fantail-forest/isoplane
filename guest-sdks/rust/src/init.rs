//! Template initialization: values an application builds once, before its
//! memory is captured, and the configuration preparation supplies to it.

use core::cell::{Cell, UnsafeCell};

use crate::abi;

/// A value built on first use. One forced by the application's initializer
/// lands in the template; any other is built by each instance that touches it.
pub struct LazyInit<T: 'static> {
    slot: UnsafeCell<Option<T>>,
    init: fn() -> T,
    /// How many times the initializer has actually executed *in this
    /// instance*. This is the number that makes instance reuse observable: it
    /// stays at 1 however many dispatches a reused instance serves.
    runs: Cell<u32>,
}

// Single logical thread: the engine has no threads and no atomics, and the
// validator rejects a module that declares them.
unsafe impl<T> Sync for LazyInit<T> {}

impl<T: 'static> LazyInit<T> {
    pub const fn new(init: fn() -> T) -> LazyInit<T> {
        LazyInit {
            slot: UnsafeCell::new(None),
            init,
            runs: Cell::new(0),
        }
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
}

impl<T: 'static> core::ops::Deref for LazyInit<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.get()
    }
}

/// Declares [`LazyInit`] statics.
///
/// ```ignore
/// lazy_init! {
///     static ROUTES: Vec<&'static str> = { vec!["/quote", "/memo"] };
/// }
/// ```
#[macro_export]
macro_rules! lazy_init {
    ($( $(#[$m:meta])* $vis:vis static $name:ident : $ty:ty = $init:block $(;)? )*) => {$(
        $(#[$m])*
        $vis static $name: $crate::init::LazyInit<$ty> =
            $crate::init::LazyInit::new(|| $init);
    )*};
}

/// Runs the application's initializer, then closes the initialization imports.
pub fn run(initialize: fn()) {
    // `abort` is available during initialization so a failure there reports usefully.
    #[cfg(target_arch = "wasm32")]
    crate::install_panic_hook();
    initialize();
    // The host answers the initialization imports with traps once an instance
    // serves; the SDK stops calling them here.
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
/// CONTRACT GAP: the blob's encoding is not declared in `isoplane-abi`. The
/// local host serializes `AppManifest::init_visible_vars` as JSON name/value
/// pairs, so that is what this decodes.
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

    static PURE: LazyInit<u32> = LazyInit::new(|| 41 + 1);

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
}
