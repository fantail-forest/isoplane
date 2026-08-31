//! The entrypoint traits (spec.md §2.1-§2.3) and the macros that emit the
//! §5.2 exports.
//!
//! Every method takes `&'static self`: there is exactly one instance of the
//! app type per wasm instance, it lives for as long as linear memory does, and
//! §5.4 guarantees the host never re-enters while a call is outstanding. App
//! state that must change goes in a `Cell`/`RefCell` field -- which is also an
//! honest reminder of §2.5.6: those fields are a best-effort per-instance
//! cache, never a store.

use core::cell::UnsafeCell;
use core::future::Future;


use crate::ctx::Ctx;
use crate::http::{Request, Response};

/// A one-per-instance slot for the app object. Filled on first dispatch and
/// then reused; a template reset zeroes it, which is exactly §2.5.6.
pub struct Slot<T: 'static>(UnsafeCell<Option<T>>);

unsafe impl<T> Sync for Slot<T> {}

impl<T: 'static> Slot<T> {
    pub const fn new() -> Slot<T> {
        Slot(UnsafeCell::new(None))
    }
    pub fn get(&'static self, init: fn() -> T) -> &'static T {
        let s = unsafe { &mut *self.0.get() };
        if s.is_none() {
            *s = Some(init());
        }
        unsafe { s.as_ref().unwrap_unchecked() }
    }
}

impl<T: 'static> Default for Slot<T> {
    fn default() -> Self {
        Slot::new()
    }
}

/// §2.2. Stateless entrypoint; lifecycle per `handler_lifecycle` (§2.6).
pub trait Handler: Sized + 'static {
    fn new() -> Self;

    fn handle(
        &'static self,
        req: Request,
        ctx: Ctx,
    ) -> impl Future<Output = Response> + 'static;


}


/// Guest GC pressure levels for `wisp_gc_hint` (§5.5).
///
/// CONTRACT GAP: `wisp-abi` types this as a bare `u8` with no enum, so these
/// values are the SDK's proposal. See guests/README.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GcPressure {
    /// Idle nudge. Coalesce, but do not write zeros: dirtying a clean page to
    /// zero it makes the delta bigger, not smaller.
    Idle = 0,
    /// Cell memory pressure. Zero what is free.
    Memory = 1,
    /// About to be captured (§6.6). Compact and zero everything reclaimable;
    /// zeroed pages compress out of the delta.
    Snapshot = 2,
}

impl GcPressure {
    pub fn from_u8(v: u8) -> GcPressure {
        match v {
            0 => GcPressure::Idle,
            1 => GcPressure::Memory,
            _ => GcPressure::Snapshot,
        }
    }
    pub fn should_zero(self) -> bool {
        !matches!(self, GcPressure::Idle)
    }
}

// ---------------------------------------------------------------------------
// Export macros
// ---------------------------------------------------------------------------

/// Emit the §5.2 exports for a [`Handler`].
///
/// ```ignore
/// wisp_guest::export_handler!(Api);
/// wisp_guest::export_handler!(Api, units = wisp_guest::init_units![FX_TABLE]);
/// ```
#[macro_export]
macro_rules! export_handler {
    ($ty:ty) => {
        $crate::export_handler!($ty, units = &[]);
    };
    ($ty:ty, units = $units:expr) => {
        #[doc(hidden)]
        static __WISP_APP: $crate::app::Slot<$ty> = $crate::app::Slot::new();
        #[doc(hidden)]
        static __WISP_UNITS: &[$crate::init::InitUnit] = $units;

        $crate::__wisp_common_exports!(__WISP_UNITS);

        /// §5.2 `wisp-handle: func(dispatch: dispatch-record) -> poll-status`,
        /// lowered as `(ptr, len) -> u64`.
        #[no_mangle]
        pub extern "C" fn wisp_handle(ptr: usize, len: usize) -> u64 {
            $crate::rt::export_handle(ptr, len, |rec| {
                use $crate::__rexport::DispatchKind;
                let ctx = $crate::Ctx::from_record(&rec);
                let app = __WISP_APP.get(<$ty as $crate::Handler>::new);
                match rec.kind {
                    DispatchKind::Http(req) => ::std::boxed::Box::pin(async move {
                        let r = <$ty as $crate::Handler>::handle(app, req, ctx).await;
                        $crate::rt::TaskOutcome::Response($crate::__rexport::into_handle(r))
                    }),
                    // A Handler cannot receive DI triggers. Say so loudly
                    // rather than silently returning done.
                }
            })
        }
    };
}


/// The exports that do not depend on the entrypoint kind.
#[macro_export]
#[doc(hidden)]
macro_rules! __wisp_common_exports {
    ($units:ident) => {
        /// §5.5 host->guest buffer handoff. The guest frees the buffer when
        /// the host hands it back in `wisp_handle` / `wisp_wake`.
        #[no_mangle]
        pub extern "C" fn wisp_alloc(size: usize, align: usize) -> usize {
            $crate::rt::export_alloc(size, align)
        }

        /// §5.2 `wisp-wake: func(future: u64, result: result-record) -> poll-status`.
        /// This is the *only* way a parked instance resumes, and it arrives on
        /// a fresh stack -- see the `wisp_guest::rt` module docs.
        #[no_mangle]
        pub extern "C" fn wisp_wake(future: u64, ptr: usize, len: usize) -> u64 {
            $crate::rt::export_wake(future, ptr, len)
        }

        /// §5.5 pre-snapshot compaction hint.
        #[no_mangle]
        pub extern "C" fn wisp_gc_hint(pressure: u32) {
            $crate::gc_hint(pressure as u8);
        }

        /// §4.4 pre-initialization, run once in the sealed init sandbox.
        #[no_mangle]
        pub extern "C" fn wisp_initialize() {
            $crate::init::run_init_units($units);
        }
    };
}
