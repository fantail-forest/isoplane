//! Host-build test support. Never compiled for `wasm32`.
//!
//! The SDK's most important property -- that parking is a `return` -- is
//! testable without a wasm engine, because the executor does not care where
//! future ids come from. These stubs stand in for the host's import vtable so
//! `cargo test` on a laptop exercises the real `rt` code paths.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::{Cell, RefCell};
use std::sync::{Mutex, MutexGuard, OnceLock};

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static CALLS: RefCell<Vec<(&'static str, Vec<i64>)>> = const { RefCell::new(Vec::new()) };
}

pub struct StubHost {
    _guard: MutexGuard<'static, ()>,
}

impl StubHost {
    /// A fresh host future id, as a real host would mint.
    pub fn next_future_id(&self) -> u64 {
        NEXT_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        })
    }
    /// Every import the SDK called, in order.
    pub fn calls(&self) -> Vec<(&'static str, Vec<i64>)> {
        CALLS.with(|c| c.borrow().clone())
    }
    pub fn called(&self, name: &str) -> bool {
        CALLS.with(|c| c.borrow().iter().any(|(n, _)| *n == name))
    }
}

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// The runtime is a process-global (there is one instance per wasm module), so
/// tests that touch it must not run concurrently.
pub fn with_stub_host<R>(f: impl FnOnce(&StubHost) -> R) -> R {
    let guard = lock().lock().unwrap_or_else(|e| e.into_inner());
    CALLS.with(|c| c.borrow_mut().clear());
    let host = StubHost { _guard: guard };
    f(&host)
}

/// Called by every generated host stub in [`crate::abi`].
pub(crate) fn stub_call(name: &'static str, args: &[i64]) -> i64 {
    CALLS.with(|c| c.borrow_mut().push((name, args.to_vec())));
    // A future-returning import hands back a future id and returns
    // immediately (§5.1). A sync import returns 0 here.
    if wisp_abi::imports::lookup(name)
        .is_some_and(|import| matches!(import.kind, wisp_abi::imports::ImportKind::Future))
    {
        NEXT_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v as i64
        })
    } else {
        0
    }
}
