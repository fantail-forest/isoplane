pub mod abi;
pub mod alloc;
pub mod app;
pub mod bindings;
pub mod budget;
pub mod ctx;
pub mod error;
pub mod future;
pub mod http;
pub mod init;
pub mod rt;

#[cfg(not(target_arch = "wasm32"))]
pub mod testing;

pub use app::{GcPressure, Handler};
pub use bindings::{fetch_bytes, log, BodyReader, Clock, Entropy, HttpFetch, Level, Span};
pub use budget::UnsafeToTrap;
pub use ctx::Ctx;
pub use error::{IsoplaneError, Result};
pub use future::{join2, join3, join_all, select2, Either, HostFuture};
pub use http::{Request, RequestExt, Response, ResponseStream};
pub use init::LazyInit;

pub use isoplane_abi;
pub use isoplane_abi::result::FutureError;

/// Items the `export_*!` macros need to name. Not a stable surface.
#[doc(hidden)]
pub mod __rexport {
    pub use isoplane_abi::dispatch::DispatchKind;
    pub use isoplane_abi::status::TrapCode;

    pub fn into_handle(r: crate::Response) -> u32 {
        r.into_handle()
    }
}

/// Called by the generated `isoplane_gc_hint` export: rolls the bump pointer
/// back over a free tail and zeroes what is reclaimable, because zeroed pages
/// compress out of the hibernation delta.
pub fn gc_hint(pressure: u8) {
    alloc::gc_hint(GcPressure::from_u8(pressure));
}

/// Current heap occupancy. Demo guests expose this so host-side delta tests
/// have a guest-visible number.
pub fn heap_stats() -> alloc::HeapStats {
    alloc::heap_stats()
}

/// Route Rust panics to the `abort` import so a fault carries a message, then
/// let the runtime trap.
///
/// The release profile builds with `panic = "abort"`, so a panic here is a
/// wasm trap and the host discards the instance -- there is no `finally`, no
/// finalizer and no cleanup quantum. A panic is not a budget kill, where a
/// dispatch can still unwind and return inside the grace window.
pub fn install_panic_hook() {
    use core::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        let b = msg.as_bytes();
        unsafe { abi::abort(b.as_ptr() as i32, b.len() as i32) };
    }));
}
