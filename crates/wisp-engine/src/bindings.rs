//! The import vtable seam (spec.md §5.3, invariant 4).
//!
//! The engine copies guest buffers out before calling the host and copies
//! results back in via `wisp_alloc`, so **no host pointer ever enters guest
//! memory** (§5.5) and the host never holds a raw pointer into a linear memory
//! that a guest could grow underneath it.

/// One invocation of a declared import.
#[derive(Debug)]
pub struct ImportCall<'a> {
    /// Name from [`wisp_abi::imports::IMPORTS`]. Guaranteed to be one of
    /// them: the linker only defines those, and anything else fails
    /// instantiation (rule V1 enforced twice).
    pub import: &'static str,
    /// The postcard-encoded argument payload, already copied out of guest
    /// memory. Empty for imports that take only scalars.
    pub payload: &'a [u8],
    /// Non-buffer scalar arguments in declaration order: handles, log levels,
    /// millisecond counts, future ids.
    pub scalars: [u64; 3],
}

/// Implemented by `wisp-host`. The engine calls this and nothing else; every
/// capability the platform offers arrives here or does not exist.
pub trait HostBindings: Send + 'static {
    /// A future-returning import (§5.1). MUST return immediately with a
    /// future id — there is no import that waits, which is what makes
    /// invariant 3 hold by construction.
    fn call_future(&mut self, call: ImportCall<'_>) -> u64;

    /// A synchronous import. Returns the raw i64/i32 result (0 for void).
    /// These must be cheap and non-blocking: they run inside the dispatch's
    /// CPU budget and hold the execution-pool thread.
    fn call_sync(&mut self, call: ImportCall<'_>) -> i64;

    /// An import whose `(ptr, len)` pair is an **output** buffer: the guest
    /// hands over space and the host fills it. `entropy_fill` and
    /// `init_hash_seed` are the two, and `call_sync`'s `i64` return has nowhere
    /// to put 16 bytes of hash seed or N bytes of CSPRNG output.
    ///
    /// The host writes into `out`, which is a **host-side** buffer; the engine
    /// copies it into linear memory afterwards, so §5.5 still holds in this
    /// direction: no host pointer enters guest memory.
    ///
    /// Returns the number of bytes written, or a negative value to deny.
    /// The default denies unsupported fill operations.
    fn call_fill(&mut self, _call: ImportCall<'_>, _out: &mut [u8]) -> i64 {
        -1
    }

    /// §8.5: the coarse clock is FROZEN for the duration of a dispatch slice
    /// and advances only at host-call/wake boundaries, which removes the
    /// measurement primitive Spectre-class attacks need. The engine asks the
    /// host for the frozen value once per slice.
    fn slice_clock_ms(&self) -> u64;

    /// Whether these bindings serve guest initialization.
    fn is_init_sandbox(&self) -> bool {
        false
    }
}
