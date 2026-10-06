//! The entrypoint traits and the macros that emit the guest exports.
//!
//! Every method takes `&'static self`: there is exactly one instance of the
//! app type per wasm instance, it lives for as long as linear memory does, and
//! the host refuses a reentrant call while one is outstanding. App state that
//! must change goes in a `Cell`/`RefCell` field -- which is also an honest
//! reminder that those fields are a best-effort per-instance cache, never a
//! store: the instance can be reset, evicted or replaced at any time.

use core::cell::UnsafeCell;
use core::future::Future;

use crate::ctx::Ctx;
use crate::http::{Request, Response};

/// A one-per-instance slot for the app object. Filled on first dispatch and
/// then reused; a template reset zeroes it, which is why nothing durable may
/// live here.
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

/// Stateless entrypoint. Whether an instance is reused between requests or
/// reset first is the app manifest's `lifecycle`.
pub trait Handler: Sized + 'static {
    fn new() -> Self;

    fn handle(&'static self, req: Request, ctx: Ctx) -> impl Future<Output = Response> + 'static;

}

/// Guest GC pressure levels for `isoplane_gc_hint`.
///
/// CONTRACT GAP: `isoplane-abi` types this as a bare `u8` with no enum, so these
/// values are the SDK's proposal. See guests/README.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GcPressure {
    /// Idle nudge. Coalesce, but do not write zeros: dirtying a clean page to
    /// zero it makes the delta bigger, not smaller.
    Idle = 0,
    /// Cell memory pressure. Zero what is free.
    Memory = 1,
    /// About to be captured. Compact and zero everything reclaimable; zeroed
    /// pages compress out of the delta.
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

/// Emit the host exports for a [`Handler`].
///
/// ```ignore
/// isoplane_guest::export_handler!(Api);
/// isoplane_guest::export_handler!(Api, units = isoplane_guest::init_units![FX_TABLE]);
/// ```
#[macro_export]
macro_rules! export_handler {
    ($ty:ty) => {
        $crate::export_handler!($ty, units = &[]);
    };
    ($ty:ty, units = $units:expr) => {
        #[doc(hidden)]
        static __ISOPLANE_APP: $crate::app::Slot<$ty> = $crate::app::Slot::new();
        #[doc(hidden)]
        static __ISOPLANE_UNITS: &[$crate::init::InitUnit] = $units;

        $crate::__isoplane_common_exports!(__ISOPLANE_UNITS);

        /// `isoplane-handle: func(dispatch: dispatch-record) -> poll-status`,
        /// lowered as `(ptr, len) -> u64`.
        #[no_mangle]
        pub extern "C" fn isoplane_handle(ptr: usize, len: usize) -> u64 {
            $crate::rt::export_handle(ptr, len, |rec| {
                use $crate::__rexport::DispatchKind;
                let ctx = $crate::Ctx::from_record(&rec);
                let app = __ISOPLANE_APP.get(<$ty as $crate::Handler>::new);
                match rec.kind {
                    DispatchKind::Http(req) => ::std::boxed::Box::pin(async move {
                        let r = <$ty as $crate::Handler>::handle(app, req, ctx).await;
                        $crate::rt::TaskOutcome::Response($crate::__rexport::into_handle(r))
                    }),
                }
            })
        }
    };
}

/// The exports that do not depend on the entrypoint kind.
#[macro_export]
#[doc(hidden)]
macro_rules! __isoplane_common_exports {
    ($units:ident) => {
        /// Host->guest buffer handoff. The guest frees the buffer when the
        /// host hands it back in `isoplane_handle` / `isoplane_wake`.
        #[no_mangle]
        pub extern "C" fn isoplane_alloc(size: usize, align: usize) -> usize {
            $crate::rt::export_alloc(size, align)
        }

        /// `isoplane-wake: func(future: u64, result: result-record) -> poll-status`.
        /// This is the *only* way a parked instance resumes, and it arrives on
        /// a fresh stack -- see the `isoplane_guest::rt` module docs.
        #[no_mangle]
        pub extern "C" fn isoplane_wake(future: u64, ptr: usize, len: usize) -> u64 {
            $crate::rt::export_wake(future, ptr, len)
        }

        /// Pre-snapshot compaction hint.
        #[no_mangle]
        pub extern "C" fn isoplane_gc_hint(pressure: u32) {
            $crate::gc_hint(pressure as u8);
        }

        /// Pre-initialization, run once in the sealed init sandbox.
        #[no_mangle]
        pub extern "C" fn isoplane_initialize() -> u64 {
            $crate::init::run_init_units($units);
            0
        }
    };
}
