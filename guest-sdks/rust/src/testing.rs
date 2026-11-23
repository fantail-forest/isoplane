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
    /// What `runtime_config_get` answers.
    static RUNTIME_CONFIG: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// Bytes an `Out(_)` import "wrote into guest memory". Host builds cannot
    /// use the passed pointer (it truncates to `i32`), so the stub parks them
    /// here and `abi::read_sized_out` copies them out.
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static CALLS: RefCell<Vec<(&'static str, Vec<i64>)>> = const { RefCell::new(Vec::new()) };
}

/// What the stub `resp_new` hands back.
pub const RESP_HANDLE: i64 = 77;

pub struct StubHost {
    _guard: MutexGuard<'static, ()>,
}

impl StubHost {
    /// Buffers allocated for the host and not yet consumed by `handle` or `wake`.
    pub fn outstanding_handoff_buffers(&self) -> usize {
        crate::rt::outstanding_handoff_buffers()
    }

    /// A fresh host future id, as a real host would mint.
    pub fn next_future_id(&self) -> u64 {
        NEXT_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        })
    }

    /// Supply the deployment settings `runtime_config_get` answers: a JSON
    /// object of string values.
    pub fn set_runtime_config(&self, json: Vec<u8>) {
        RUNTIME_CONFIG.with(|c| *c.borrow_mut() = json);
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
    NEXT_ID.with(|c| c.set(1));
    RUNTIME_CONFIG.with(|c| *c.borrow_mut() = b"{}".to_vec());
    OUT.with(|o| o.borrow_mut().clear());
    // The executor is a module-global; start every test from a clean one.
    crate::rt::reset();
    let host = StubHost { _guard: guard };
    f(&host)
}

/// Collect what the last `Out(_)` stub produced. See [`OUT`].
pub(crate) fn take_out_buffer(dst: &mut [u8]) {
    OUT.with(|o| {
        let src = o.borrow();
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
    });
}

/// Called by every generated host stub in [`crate::abi`].
pub(crate) fn stub_call(name: &'static str, args: &[i64]) -> i64 {
    CALLS.with(|c| c.borrow_mut().push((name, args.to_vec())));
    // A future-returning import hands back a future id and returns
    // immediately. A sync import returns 0 here.
    // `resp_new` is the one sync import whose return value the tests care
    // about: it is the response handle `PollStatus::Done` carries.
    if name == "resp_new" {
        return RESP_HANDLE;
    }
    // Writes up to `cap` bytes and returns the full length, as the host does,
    // so a zero-length probe sizes the buffer.
    if name == "runtime_config_get" {
        return RUNTIME_CONFIG.with(|c| {
            let blob = c.borrow();
            let cap = args[1].max(0) as usize;
            let n = cap.min(blob.len());
            OUT.with(|o| *o.borrow_mut() = blob[..n].to_vec());
            blob.len() as i64
        });
    }
    if isoplane_abi::imports::lookup(name)
        .is_some_and(|import| matches!(import.kind, isoplane_abi::imports::ImportKind::Future))
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
