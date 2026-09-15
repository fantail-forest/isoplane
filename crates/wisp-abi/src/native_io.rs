//! Typed requests between the canonical socket adapter and host-owned resources.

pub mod canonical;
pub mod network_error;

use std::net::SocketAddr;

/// An instance-local resource name, never an operating-system descriptor.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceHandle(pub u32);

/// A duration expressed in the canonical WASI clock unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationNanos(pub u64);

/// The IP family requested when creating an outbound socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    /// IPv4 only.
    V4,
    /// IPv6 only.
    V6,
}

/// Resource type required by a canonical destructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// Outbound TCP socket.
    Tcp,
    /// Socket input stream.
    Input,
    /// Socket output stream.
    Output,
    /// An asynchronous address lookup.
    Addresses,
    /// An observed stream failure.
    Error,
    /// Deadline or readiness subscription.
    Pollable,
}

/// Bytes requested or admitted by one nonblocking stream operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteCount(pub u64);

/// A nonblocking operation on resources owned by the current guest instance.
#[derive(Debug)]
pub enum Request {
    /// Read or set TCP_NODELAY; a pre-connect setting applies before application I/O.
    NoDelay {
        /// Instance-owned TCP socket.
        socket: ResourceHandle,
        /// New setting, or None to read the current setting.
        enabled: Option<bool>,
    },
    /// Acquire the instance's network capability handle.
    Network,
    /// Resolve a name using the host's configured DNS service.
    Resolve {
        /// Instance network capability.
        network: ResourceHandle,
        /// DNS name to authorize and resolve.
        name: String,
    },
    /// Take the next address from a completed lookup.
    NextAddress(ResourceHandle),
    /// Create an unconnected TCP resource.
    CreateTcp(AddressFamily),
    /// Start an authorized connection on the host reactor.
    Connect {
        /// Socket receiving the connection.
        socket: ResourceHandle,
        /// Network capability established by the host.
        network: ResourceHandle,
        /// Exact address to authorize and connect.
        destination: SocketAddr,
    },
    /// Obtain stream handles after a connection finishes.
    FinishConnect(ResourceHandle),
    /// Read currently available input, returning an empty chunk while pending.
    Read {
        /// Owned input stream.
        stream: ResourceHandle,
        /// Maximum bytes to return.
        length: ByteCount,
    },
    /// Send currently writable bytes, preserving POSIX partial-write semantics.
    TrySend {
        /// Pollables collected from the libc descriptor through its public poll interface.
        pollables: Vec<ResourceHandle>,
        /// Bytes offered to the host socket.
        bytes: Vec<u8>,
    },
    /// Inspect the message on an owned stream failure.
    ErrorMessage(ResourceHandle),
    /// Inspect the network error code of an owned stream failure.
    ErrorCode(ResourceHandle),
    /// Read the actual local or remote endpoint of a connected socket.
    SocketAddress {
        /// Owned socket.
        socket: ResourceHandle,
        /// Select the remote endpoint when true.
        peer: bool,
    },
    /// Shut down one or both directions of a connected socket.
    Shutdown {
        /// Owned socket.
        socket: ResourceHandle,
        /// Directions closed by the caller.
        how: std::net::Shutdown,
    },
    /// Subscribe to readiness of a socket or stream.
    Subscribe(ResourceHandle),
    /// Create a monotonic deadline pollable.
    Timer(DurationNanos),
    /// Inspect readiness without waiting on the guest stack.
    Ready(ResourceHandle),
    /// Return indexes of ready pollables, arranging a host wake for pending ones.
    Poll(Vec<ResourceHandle>),
    /// Release the resource and cancel work that it owns.
    Drop {
        /// Resource being released.
        handle: ResourceHandle,
        /// Type established by the canonical destructor.
        kind: ResourceKind,
    },
}

/// The complete result of a nonblocking resource operation.
#[derive(Debug)]
pub enum Response {
    /// The socket's effective TCP_NODELAY setting.
    NoDelay(bool),
    /// The operation completed without a value.
    Done,
    /// A newly allocated or subscribed resource.
    Handle(ResourceHandle),
    /// Input and output streams for an established connection.
    Streams {
        /// Read side.
        input: ResourceHandle,
        /// Write side.
        output: ResourceHandle,
    },
    /// The actual endpoint of a connected socket.
    SocketAddress(SocketAddr),
    /// The next resolved IP address, or the end of the answer.
    Address(Option<std::net::IpAddr>),
    /// Currently available input; an empty chunk is temporarily pending.
    Bytes(Vec<u8>),
    /// Bytes accepted by the current output write.
    Capacity(ByteCount),
    /// The stream has reached its terminal end.
    Closed,
    /// An owned error record describing a failed stream operation.
    StreamError(ResourceHandle),
    /// The network error code an owned error record carries.
    ErrorCode(network_error::NetworkErrorCode),
    /// Current readiness of one resource.
    Ready(bool),
    /// Indexes into the caller's poll list.
    ReadyIndexes(Vec<u32>),
}

/// A resource refusal or asynchronous connection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The destination or operation is not authorized.
    #[error("outbound operation denied: {0}")]
    Denied(String),
    /// A canonical operation has no supported implementation.
    #[error("unsupported outbound operation: {0}")]
    Unsupported(&'static str),
    /// The supplied handle is not owned by the calling instance.
    #[error("resource handle {0:?} is not owned by this instance")]
    InvalidHandle(ResourceHandle),
    /// The operation is incompatible with the resource's current state.
    #[error("invalid outbound resource state: {0}")]
    InvalidState(&'static str),
    /// An asynchronous operation has not completed.
    #[error("outbound operation is still pending")]
    WouldBlock,
    /// A host-owned resource budget is exhausted.
    #[error("outbound resource limit reached")]
    ResourceLimit,
    /// A name could not be resolved by the configured DNS service.
    #[error("outbound DNS failed ({kind:?}): {message}")]
    Dns {
        /// Resolver failure classification.
        kind: DnsFailure,
        /// Resolver diagnostic.
        message: String,
    },
    /// The operating system refused or terminated the connection.
    #[error("outbound connection failed ({kind:?}): {message}")]
    Connection {
        /// Operating-system failure category, retained for canonical error translation.
        kind: std::io::ErrorKind,
        /// Diagnostic supplied by the operating system.
        message: String,
    },
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Connection {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

impl Error {
    /// The `wasi:sockets/network` error code this failure is reported as,
    /// both in a failed call's result and through `network-error-code` on
    /// the error record a failed stream operation leaves behind.
    pub fn network_error_code(&self) -> network_error::NetworkErrorCode {
        use network_error::NetworkErrorCode;
        match self {
            Error::Denied(_) => NetworkErrorCode::AccessDenied,
            Error::Unsupported(_) => NetworkErrorCode::NotSupported,
            Error::InvalidHandle(_) => NetworkErrorCode::InvalidArgument,
            Error::InvalidState(_) => NetworkErrorCode::InvalidState,
            Error::WouldBlock => NetworkErrorCode::WouldBlock,
            Error::ResourceLimit => NetworkErrorCode::NewSocketLimit,
            Error::Dns { kind, .. } => match kind {
                DnsFailure::NotFound => NetworkErrorCode::NameUnresolvable,
                DnsFailure::Temporary => NetworkErrorCode::TemporaryResolverFailure,
                DnsFailure::Permanent => NetworkErrorCode::PermanentResolverFailure,
                DnsFailure::Timeout => NetworkErrorCode::Timeout,
            },
            Error::Connection { kind, .. } => match kind {
                std::io::ErrorKind::PermissionDenied => NetworkErrorCode::AccessDenied,
                std::io::ErrorKind::TimedOut => NetworkErrorCode::Timeout,
                std::io::ErrorKind::AddrNotAvailable => NetworkErrorCode::AddressNotBindable,
                std::io::ErrorKind::AddrInUse => NetworkErrorCode::AddressInUse,
                std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable => {
                    NetworkErrorCode::RemoteUnreachable
                }
                std::io::ErrorKind::ConnectionRefused => NetworkErrorCode::ConnectionRefused,
                std::io::ErrorKind::ConnectionReset => NetworkErrorCode::ConnectionReset,
                std::io::ErrorKind::ConnectionAborted => NetworkErrorCode::ConnectionAborted,
                // Other OS failures have no more specific WASI network equivalent.
                _ => NetworkErrorCode::Unknown,
            },
        }
    }
}

/// Portable DNS failure classifications retained across the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsFailure {
    /// The name has no address records or does not exist.
    NotFound,
    /// The resolver can be retried after a transient service failure.
    Temporary,
    /// The resolver could not process the query.
    Permanent,
    /// The resolver did not answer within its configured deadline.
    Timeout,
}

/// Benign native-resource identities retained by a prepared guest; no live I/O.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedResources {
    /// First unused handle; identities are never reused within an instance.
    pub next_handle: u32,
    /// Guest-cached network authority, rebound to the activating host's policy.
    pub network: Option<ResourceHandle>,
    /// Immediately ready pollables with no timer task or wake authority.
    pub ready_pollables: Vec<ResourceHandle>,
}

impl Default for PreparedResources {
    fn default() -> Self {
        Self {
            next_handle: 1,
            network: None,
            ready_pollables: Vec::new(),
        }
    }
}

/// Portable template section carrying preparation's closed native-resource state.
pub const PREPARED_RESOURCES_SECTION: &str = "wisp.prepared-resources";
