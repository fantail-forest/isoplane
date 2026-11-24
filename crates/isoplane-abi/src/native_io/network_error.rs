/// The wasi:sockets/network@0.2.0 error codes the host reports, with that
/// interface's discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NetworkErrorCode {
    /// No more specific classification is available.
    Unknown = 0,
    /// The operation was denied.
    AccessDenied = 1,
    /// The operation is unsupported.
    NotSupported = 2,
    /// An argument is invalid.
    InvalidArgument = 3,
    /// The operation timed out.
    Timeout = 5,
    /// The operation is pending.
    WouldBlock = 8,
    /// The resource state does not permit this operation.
    InvalidState = 9,
    /// The socket limit is reached.
    NewSocketLimit = 10,
    /// The address cannot be bound.
    AddressNotBindable = 11,
    /// The address is in use.
    AddressInUse = 12,
    /// The remote endpoint is unreachable.
    RemoteUnreachable = 13,
    /// The remote endpoint refused the connection.
    ConnectionRefused = 14,
    /// The peer reset the connection.
    ConnectionReset = 15,
    /// The connection was aborted.
    ConnectionAborted = 16,
    /// The name has no address.
    NameUnresolvable = 18,
    /// The resolver failed temporarily.
    TemporaryResolverFailure = 19,
    /// The resolver failed permanently.
    PermanentResolverFailure = 20,
}
