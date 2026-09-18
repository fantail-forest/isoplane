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
    /// Unit ids the "host" has already learned are impure.
    static DEFERRED: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
    /// The init-visible config blob this deploy supplies.
    static CONFIG: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
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
    /// A fresh host future id, as a real host would mint.
    pub fn next_future_id(&self) -> u64 {
        NEXT_ID.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        })
    }
    /// Mark a pre-init unit as one the host discovered was impure on a
    /// previous pass, so the replay must skip it.
    pub fn defer_unit(&self, id: i64) {
        DEFERRED.with(|d| d.borrow_mut().push(id));
    }

    /// Supply the init-visible config blob the sealed init sandbox will
    /// serve. The control plane encodes it as JSON `[(name, value), ..]`.
    pub fn set_config(&self, blob: Vec<u8>) {
        CONFIG.with(|c| *c.borrow_mut() = blob);
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
    DEFERRED.with(|d| d.borrow_mut().clear());
    CONFIG.with(|c| c.borrow_mut().clear());
    OUT.with(|o| o.borrow_mut().clear());
    // The executor and the pre-init state are module-globals; start every
    // test from a clean one.
    crate::rt::reset();
    crate::init::reset_for_test();
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
    // Pre-init replay: non-zero means "this unit is deferred, skip it".
    if name == "init_unit_deferred" {
        return DEFERRED.with(|d| i64::from(d.borrow().contains(&args[0])));
    }
    // Config blob: write up to `cap` bytes, return the FULL length so a
    // zero-length probe sizes the buffer. Mirrors the host in
    // `isoplane_control::preinit`.
    if name == "init_config_get" {
        return CONFIG.with(|c| {
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
