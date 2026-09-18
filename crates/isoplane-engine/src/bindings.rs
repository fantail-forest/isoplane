//! The import vtable seam. The engine copies guest buffers out before calling
//! the host and copies results back in through `isoplane_alloc`, so no host
//! pointer enters guest memory and the host never holds a pointer into a
//! linear memory the guest could grow underneath it.

/// One invocation of a declared import.
#[derive(Debug)]
pub struct ImportCall<'a> {
    /// A name from [`isoplane_abi::imports::IMPORTS`]; the linker defines nothing
    /// else.
    pub import: &'static str,
    /// The postcard-encoded argument payload, already copied out of guest
    /// memory. Empty for imports that take only scalars.
    pub payload: &'a [u8],
    /// Non-buffer scalar arguments in declaration order.
    pub scalars: [u64; 3],
}

/// Implemented by `isoplane-host`. Every capability the platform offers arrives
/// here or does not exist.
pub trait HostBindings: Send + 'static {
    /// Restore template resource identities before guest code runs, preserving already-owned
    /// resources when rebinding the same template after Store destruction.
    fn restore_prepared_resources(
        &mut self,
        resources: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), BindingError> {
        if resources == &isoplane_abi::native_io::PreparedResources::default() {
            Ok(())
        } else {
            Err(BindingError::Unsupported("prepared native resources"))
        }
    }

    /// Discard request-owned resources and restore the template baseline for fresh reuse.
    fn reset_prepared_resources(
        &mut self,
        resources: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), BindingError> {
        self.restore_prepared_resources(resources)
    }

    /// Per-instance linear-memory ceiling, bounded by the engine capacity.
    /// None uses the engine capacity.
    fn memory_limit_bytes(&self) -> Option<u64> {
        None
    }

    /// Perform a nonblocking native resource operation for this instance.
    /// Hosts without native readiness support refuse the operation explicitly.
    fn native_io(
        &mut self,
        _request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        Err(isoplane_abi::native_io::Error::Unsupported(
            "native I/O host binding",
        ))
    }

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

/// A synchronous binding refusal that the engine converts into a guest trap.
#[derive(Debug, thiserror::Error)]
pub enum BindingError {
    /// The execution context cannot perform this operation.
    #[error("unsupported host binding: {0}")]
    Unsupported(&'static str),
    /// The guest supplied invalid arguments for an operation.
    #[error("invalid host binding request: {0}")]
    InvalidRequest(&'static str),
}

/// Fallible initialization operations, with no facility for creating host futures or resources.
pub trait InitializationBindings: Send + 'static {
    /// Whether application initialization may draw ambient entropy.
    fn permits_entropy(&self) -> bool {
        false
    }

    /// Whether this context admits asynchronous native capabilities outside init units.
    fn permits_io(&self) -> bool {
        false
    }
    /// Execute a nonblocking native operation under preparation's outbound policy.
    fn native_io(
        &mut self,
        _request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        Err(isoplane_abi::native_io::Error::Denied(
            "initialization I/O is disabled".into(),
        ))
    }
    /// Frozen preparation clock advanced at each returned slice.
    fn slice_clock_ms(&self) -> u64 {
        0
    }

    /// Application memory ceiling during initialization; None uses engine capacity.
    fn memory_limit_bytes(&self) -> Option<u64> {
        None
    }

    /// Answer an init-visible scalar operation or return a typed refusal.
    fn call_sync(&mut self, call: ImportCall<'_>) -> Result<i64, BindingError>;
    /// Fill an init-visible output buffer, returning its full logical length.
    fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> Result<i64, BindingError>;
    /// Whether the additional runtime-initialization capability policy is enabled.
    fn runtime_bringup(&self) -> bool;
}

pub(crate) enum Bindings {
    Runtime(Box<dyn HostBindings>),
    Initialization(Box<dyn InitializationBindings>),
}

impl Bindings {
    /// Whether the active initializer permits ambient entropy.
    pub fn permits_initialization_entropy(&self) -> bool {
        match self {
            Self::Runtime(_) => false,
            Self::Initialization(bindings) => bindings.permits_entropy(),
        }
    }

    pub fn permits_initialization_io(&self) -> bool {
        match self {
            Self::Runtime(_) => false,
            Self::Initialization(bindings) => bindings.permits_io(),
        }
    }
    pub fn restore_prepared_resources(
        &mut self,
        resources: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), BindingError> {
        match self {
            Self::Runtime(bindings) => bindings.restore_prepared_resources(resources),
            Self::Initialization(_) => Ok(()),
        }
    }

    pub fn reset_prepared_resources(
        &mut self,
        resources: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), BindingError> {
        match self {
            Self::Runtime(bindings) => bindings.reset_prepared_resources(resources),
            Self::Initialization(_) => Ok(()),
        }
    }

    pub fn is_init_sandbox(&self) -> bool {
        match self {
            Self::Runtime(bindings) => bindings.is_init_sandbox(),
            Self::Initialization(_) => true,
        }
    }
    pub fn init_runtime_bringup(&self) -> bool {
        match self {
            Self::Runtime(_) => false,
            Self::Initialization(bindings) => bindings.runtime_bringup(),
        }
    }
    pub fn slice_clock_ms(&self) -> u64 {
        match self {
            Self::Runtime(bindings) => bindings.slice_clock_ms(),
            Self::Initialization(bindings) => bindings.slice_clock_ms(),
        }
    }
    pub fn enter_grace(&mut self) {
        match self {
            Self::Runtime(bindings) => bindings.enter_grace(),
            Self::Initialization(_) => {}
        }
    }
    pub fn native_io(
        &mut self,
        request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        match self {
            Self::Runtime(bindings) => bindings.native_io(request),
            Self::Initialization(bindings) => bindings.native_io(request),
        }
    }
    pub fn call_future(&mut self, call: ImportCall<'_>) -> Result<u64, BindingError> {
        match self {
            Self::Runtime(bindings) => Ok(bindings.call_future(call)),
            Self::Initialization(_) => Err(BindingError::Unsupported(call.import)),
        }
    }
    pub fn call_sync(&mut self, call: ImportCall<'_>) -> Result<i64, BindingError> {
        match self {
            Self::Runtime(bindings) => Ok(bindings.call_sync(call)),
            Self::Initialization(bindings) => bindings.call_sync(call),
        }
    }
    pub fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> Result<i64, BindingError> {
        match self {
            Self::Runtime(bindings) => Ok(bindings.call_fill(call, out)),
            Self::Initialization(bindings) => bindings.call_fill(call, out),
        }
    }
}
