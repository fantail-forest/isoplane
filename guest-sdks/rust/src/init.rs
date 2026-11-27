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

/// Runs the application's initializer.
pub fn run(initialize: fn()) {
    // `abort` is available during initialization so a failure there reports usefully.
    #[cfg(target_arch = "wasm32")]
    crate::install_panic_hook();
    initialize();
}

/// The deployment's settings as the host supplies them: a JSON object of
/// string values. Values can be secret; one an initializer keeps is captured
/// in the template.
pub fn runtime_settings_json() -> Vec<u8> {
    let size = abi::read_sized_out(abi::runtime_config_get, &mut []);
    let mut json = vec![0u8; usize::try_from(size).unwrap_or(0)];
    abi::read_sized_out(abi::runtime_config_get, &mut json);
    json
}

/// [`runtime_settings_json`] by name.
///
/// Panics when the host's answer is not a JSON object of string values.
#[cfg(feature = "json")]
pub fn runtime_settings() -> std::collections::BTreeMap<String, String> {
    serde_json::from_slice(&runtime_settings_json()).unwrap_or_else(|error| {
        panic!("runtime configuration is not a JSON object of string values: {error}")
    })
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
