//! Canonical WASI network error values shared by the engine and managed adapter.

macro_rules! errors {
    ($($name:ident = $value:literal, $doc:literal;)*) => {
        /// Error discriminants in wasi:sockets/network@0.2.0.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum NetworkErrorCode {
            $(#[doc = $doc] $name = $value,)*
        }

        /// Generate the managed representation of canonical network errors.
        pub fn managed_errors() -> String {
            let mut output = String::from("// Generated from wisp_abi::native_io::network_error.\nnamespace Wisp.Abi;\n\ninternal enum NetworkErrorCode\n{\n");
            $(output.push_str(&format!("    {} = {},\n", stringify!($name), $value));)*
            output.push_str("}\n");
            output
        }
    };
}

errors! {
    Unknown = 0, "No more specific classification is available.";
    AccessDenied = 1, "The operation was denied.";
    NotSupported = 2, "The operation is unsupported.";
    InvalidArgument = 3, "An argument is invalid.";
    OutOfMemory = 4, "Memory is exhausted.";
    Timeout = 5, "The operation timed out.";
    ConcurrencyConflict = 6, "Another operation conflicts with this one.";
    NotInProgress = 7, "No operation is in progress.";
    WouldBlock = 8, "The operation is pending.";
    InvalidState = 9, "The resource state does not permit this operation.";
    NewSocketLimit = 10, "The socket limit is reached.";
    AddressNotBindable = 11, "The address cannot be bound.";
    AddressInUse = 12, "The address is in use.";
    RemoteUnreachable = 13, "The remote endpoint is unreachable.";
    ConnectionRefused = 14, "The remote endpoint refused the connection.";
    ConnectionReset = 15, "The peer reset the connection.";
    ConnectionAborted = 16, "The connection was aborted.";
    DatagramTooLarge = 17, "The datagram exceeds its size limit.";
    NameUnresolvable = 18, "The name has no address.";
    TemporaryResolverFailure = 19, "The resolver failed temporarily.";
    PermanentResolverFailure = 20, "The resolver failed permanently.";
}

#[cfg(test)]
mod tests {
    #[test]
    fn managed_errors_match_the_canonical_network_errors() {
        assert_eq!(
            super::managed_errors(),
            include_str!("../../../../guest-sdks/dotnet/Abi/NetworkErrorCode.cs"),
            "regenerate managed network errors with the native_network_errors example"
        );
    }
}
