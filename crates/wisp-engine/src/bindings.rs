//! The import vtable seam. The engine copies guest buffers out before calling
//! the host and copies results back in through `wisp_alloc`, so no host
//! pointer enters guest memory and the host never holds a pointer into a
//! linear memory the guest could grow underneath it.

/// One invocation of a declared import.
#[derive(Debug)]
pub struct ImportCall<'a> {
    /// A name from [`wisp_abi::imports::IMPORTS`]; the linker defines nothing
    /// else.
    pub import: &'static str,
    /// The postcard-encoded argument payload, already copied out of guest
    /// memory. Empty for imports that take only scalars.
    pub payload: &'a [u8],
    /// Non-buffer scalar arguments in declaration order.
    pub scalars: [u64; 3],
}

/// Implemented by `wisp-host`. Every capability the platform offers arrives
/// here or does not exist.
pub trait HostBindings: Send + 'static {
    /// A future-returning import. Returns the future id immediately; no
    /// import waits.
    fn call_future(&mut self, call: ImportCall<'_>) -> u64;

    /// A synchronous import. Returns the raw result, 0 for void. Runs inside
    /// the dispatch's CPU budget on the execution-pool thread.
    fn call_sync(&mut self, call: ImportCall<'_>) -> i64;

    /// An import whose `(ptr, cap)` pair is an out-buffer. The host writes
    /// into `out`, a host-side buffer the engine copies into linear memory
    /// afterwards, and returns the value's full length or a negative value to
    /// deny. The default denies unsupported fill operations.
    fn call_fill(&mut self, _call: ImportCall<'_>, _out: &mut [u8]) -> i64 {
        -1
    }

    /// The coarse clock sampled at slice entry and after future-returning imports.
    /// Serving bindings keep this value fixed throughout their slice.
    fn slice_clock_ms(&self) -> u64;

    /// Whether these bindings serve guest initialization.
    fn is_init_sandbox(&self) -> bool {
        false
    }

    /// The dispatch crossed B. Called from the epoch deadline callback on the
    /// execution-pool thread with the guest suspended at a back-edge, once per
    /// slice. The host completes the dispatch's cancellation future and
    /// poisons its imports so every later call returns `Cancelled`. Must not
    /// re-enter the guest. The default offers no grace, so every over-budget
    /// dispatch takes the hard trap at B+G.
    fn enter_grace(&mut self) {}
}
