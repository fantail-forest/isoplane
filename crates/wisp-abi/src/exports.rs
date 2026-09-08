//! The guest exports the host requires, and their core-wasm signatures.

pub const ALLOC: &str = "wisp_alloc";
pub const HANDLE: &str = "wisp_handle";
pub const WAKE: &str = "wisp_wake";
pub const GC_HINT: &str = "wisp_gc_hint";
pub const INITIALIZE: &str = "wisp_initialize";
/// Allocate a guest-owned filesystem record through the guest allocator.
pub const FS_ALLOC: &str = "wisp_fs_alloc";
/// Release a guest-owned filesystem record through the guest allocator.
pub const FS_FREE: &str = "wisp_fs_free";
pub const MEMORY: &str = "memory";

pub const REQUIRED: &[&str] = &[ALLOC, HANDLE, WAKE, MEMORY];
pub const OPTIONAL: &[&str] = &[GC_HINT, INITIALIZE, FS_ALLOC, FS_FREE];

use crate::imports::{Sig, Ty::I32, Ty::I64};

/// `(ptr, len)` pairs address a postcard-encoded `DispatchRecord` or
/// `ResultRecord` in guest memory; `wisp_handle` and `wisp_wake` return a
/// packed [`crate::status::PollStatus`].
pub const SIG_ALLOC: Sig = Sig {
    params: &[I32, I32],
    results: &[I32],
};
pub const SIG_HANDLE: Sig = Sig {
    params: &[I32, I32],
    results: &[I64],
};
pub const SIG_WAKE: Sig = Sig {
    params: &[I64, I32, I32],
    results: &[I64],
};
pub const SIG_GC_HINT: Sig = Sig {
    params: &[I32],
    results: &[],
};
pub const SIG_INITIALIZE: Sig = Sig {
    params: &[],
    results: &[],
};

pub const SIGNATURES: &[(&str, Sig)] = &[
    (ALLOC, SIG_ALLOC),
    (HANDLE, SIG_HANDLE),
    (WAKE, SIG_WAKE),
    (GC_HINT, SIG_GC_HINT),
    (INITIALIZE, SIG_INITIALIZE),
    (
        FS_ALLOC,
        Sig {
            params: &[I32],
            results: &[I32],
        },
    ),
    (
        FS_FREE,
        Sig {
            params: &[I32],
            results: &[],
        },
    ),
];

pub fn signature(name: &str) -> Option<Sig> {
    SIGNATURES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}
