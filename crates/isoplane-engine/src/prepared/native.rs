//! Native code shared by every template whose checked local manifest records
//! the same compiled payload, so an engine holds one copy per distinct
//! runtime however many applications it serves.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// One deserialized native module. Templates hold it strongly and the
/// engine's index weakly, so it lives exactly as long as a template uses it.
pub(crate) struct SharedNative {
    pub(crate) module: wasmtime::Module,
}

/// Native modules this engine has deserialized, by the BLAKE3 digest of the
/// payload bytes they were verified and deserialized from.
#[derive(Default)]
pub(crate) struct NativeIndex(parking_lot::Mutex<HashMap<[u8; 32], Weak<SharedNative>>>);

impl NativeIndex {
    /// The module a live template already holds for `hash`.
    pub(crate) fn get(&self, hash: &[u8; 32]) -> Option<Arc<SharedNative>> {
        let mut modules = self.0.lock();
        let native = modules.get(hash).and_then(Weak::upgrade);
        if native.is_none() {
            modules.remove(hash);
        }
        native
    }

    /// Indexes a module deserialized from verified bytes that hashed to
    /// `hash`. When a concurrent load indexed the same payload first, returns
    /// that module and drops this one.
    pub(crate) fn insert(&self, hash: [u8; 32], module: wasmtime::Module) -> Arc<SharedNative> {
        let mut modules = self.0.lock();
        if let Some(native) = modules.get(&hash).and_then(Weak::upgrade) {
            return native;
        }
        modules.retain(|_, native| native.strong_count() > 0);
        let native = Arc::new(SharedNative { module });
        modules.insert(hash, Arc::downgrade(&native));
        native
    }
}
