//! Typed requests between the canonical `wasi:http` adapter and the host's
//! outbound HTTP resources: header fields, an outgoing request and its
//! body, the future response, the incoming response and its body.

use super::ResourceHandle;

/// A header name and its value bytes, as `wasi:http` fields carry them.
pub type Field = (String, Vec<u8>);

/// The `wasi:http` request method variant, by its canonical discriminant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// GET.
    Get,
    /// HEAD.
    Head,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// DELETE.
    Delete,
    /// CONNECT.
    Connect,
    /// OPTIONS.
    Options,
    /// TRACE.
    Trace,
    /// PATCH.
    Patch,
    /// Any other token.
    Other(String),
}

impl Method {
    /// The method's HTTP token.
    pub fn as_str(&self) -> &str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Patch => "PATCH",
            Method::Other(token) => token,
        }
    }
}

/// The `wasi:http` scheme variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS, terminated at the host.
    Https,
    /// Any other scheme, which the host refuses.
    Other(String),
}

/// The `wasi:http` `header-error` variant, by canonical discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HeaderError {
    /// The name or value is not a valid header.
    InvalidSyntax = 0,
    /// The header may not be set by a guest.
    Forbidden = 1,
    /// The fields are immutable.
    Immutable = 2,
}

/// The `wasi:http` `error-code` variant cases the host reports, by canonical
/// discriminant. Cases with a payload are reported without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    /// The resolver did not answer in time.
    DnsTimeout = 0,
    /// The resolver failed or found no address.
    DnsError = 1,
    /// The destination does not exist.
    DestinationNotFound = 2,
    /// The destination could not be reached.
    DestinationUnavailable = 3,
    /// The destination address is one the platform never dials.
    DestinationIpProhibited = 4,
    /// The destination address is unroutable.
    DestinationIpUnroutable = 5,
    /// The destination refused the connection.
    ConnectionRefused = 6,
    /// The connection ended before the exchange completed.
    ConnectionTerminated = 7,
    /// Connecting timed out.
    ConnectionTimeout = 8,
    /// Reading timed out.
    ConnectionReadTimeout = 9,
    /// Writing timed out.
    ConnectionWriteTimeout = 10,
    /// The connection quota is exhausted.
    ConnectionLimitReached = 11,
    /// TLS failed for a reason other than the certificate.
    TlsProtocolError = 12,
    /// The server certificate is not trusted or does not match.
    TlsCertificateError = 13,
    /// The peer sent a TLS alert.
    TlsAlertReceived = 14,
    /// The request was refused by policy.
    HttpRequestDenied = 15,
    /// The request needs a length.
    HttpRequestLengthRequired = 16,
    /// The request body exceeds a limit.
    HttpRequestBodySize = 17,
    /// The method is not valid.
    HttpRequestMethodInvalid = 18,
    /// The URI is not valid.
    HttpRequestUriInvalid = 19,
    /// The URI is too long.
    HttpRequestUriTooLong = 20,
    /// The request headers exceed a limit.
    HttpRequestHeaderSectionSize = 21,
    /// One request header exceeds a limit.
    HttpRequestHeaderSize = 22,
    /// The request trailers exceed a limit.
    HttpRequestTrailerSectionSize = 23,
    /// One request trailer exceeds a limit.
    HttpRequestTrailerSize = 24,
    /// The response ended early.
    HttpResponseIncomplete = 25,
    /// The response headers exceed a limit.
    HttpResponseHeaderSectionSize = 26,
    /// One response header exceeds a limit.
    HttpResponseHeaderSize = 27,
    /// The response body exceeds a limit.
    HttpResponseBodySize = 28,
    /// The response trailers exceed a limit.
    HttpResponseTrailerSectionSize = 29,
    /// One response trailer exceeds a limit.
    HttpResponseTrailerSize = 30,
    /// The response transfer coding is unsupported.
    HttpResponseTransferCoding = 31,
    /// The response content coding is unsupported.
    HttpResponseContentCoding = 32,
    /// The response did not arrive in time.
    HttpResponseTimeout = 33,
    /// A protocol upgrade failed.
    HttpUpgradeFailed = 34,
    /// The peer violated the protocol.
    HttpProtocolError = 35,
    /// A request loop was detected.
    LoopDetected = 36,
    /// The host is misconfigured.
    ConfigurationError = 37,
    /// Any other host failure.
    InternalError = 38,
}

/// An operation on the instance's `wasi:http` resources.
#[derive(Debug)]
pub enum Request {
    /// A new, empty, mutable field set.
    FieldsNew,
    /// A new field set from entries; an invalid entry fails the whole set.
    FieldsFromList(Vec<Field>),
    /// Every value of a name.
    FieldsGet {
        /// Owned field set.
        fields: ResourceHandle,
        /// Header name.
        name: String,
    },
    /// Whether a name has a value.
    FieldsHas {
        /// Owned field set.
        fields: ResourceHandle,
        /// Header name.
        name: String,
    },
    /// Replace every value of a name.
    FieldsSet {
        /// Owned field set.
        fields: ResourceHandle,
        /// Header name.
        name: String,
        /// New values.
        values: Vec<Vec<u8>>,
    },
    /// Remove every value of a name.
    FieldsDelete {
        /// Owned field set.
        fields: ResourceHandle,
        /// Header name.
        name: String,
    },
    /// Add one value of a name.
    FieldsAppend {
        /// Owned field set.
        fields: ResourceHandle,
        /// Header name.
        name: String,
        /// Value to add.
        value: Vec<u8>,
    },
    /// Every entry, in order.
    FieldsEntries(ResourceHandle),
    /// A mutable copy.
    FieldsClone(ResourceHandle),
    /// A new outgoing request taking ownership of its headers.
    RequestNew {
        /// Owned field set, consumed.
        headers: ResourceHandle,
    },
    /// The request's body, once.
    RequestBody(ResourceHandle),
    /// Set the method.
    RequestSetMethod {
        /// Owned request.
        request: ResourceHandle,
        /// Method to send.
        method: Method,
    },
    /// Set or clear the scheme.
    RequestSetScheme {
        /// Owned request.
        request: ResourceHandle,
        /// Scheme, or none.
        scheme: Option<Scheme>,
    },
    /// Set or clear the authority.
    RequestSetAuthority {
        /// Owned request.
        request: ResourceHandle,
        /// `host[:port]`, or none.
        authority: Option<String>,
    },
    /// Set or clear the path and query.
    RequestSetPath {
        /// Owned request.
        request: ResourceHandle,
        /// Path with query, or none.
        path: Option<String>,
    },
    /// A read-only view of the request's headers.
    RequestHeaders(ResourceHandle),
    /// The body's output stream, once.
    OutgoingBodyWrite(ResourceHandle),
    /// End the body; the request is complete once it is sent.
    OutgoingBodyFinish {
        /// Owned body, consumed.
        body: ResourceHandle,
        /// Trailers, which the host does not send.
        trailers: Option<ResourceHandle>,
    },
    /// Issue the request under the egress policy; consumes the request.
    Handle {
        /// Owned request, consumed.
        request: ResourceHandle,
        /// Options, which the host does not support.
        options: Option<ResourceHandle>,
    },
    /// A readiness subscription on the future response.
    FutureSubscribe(ResourceHandle),
    /// The response once it arrived, or that it is pending.
    FutureGet(ResourceHandle),
    /// The status code.
    ResponseStatus(ResourceHandle),
    /// The response headers, as an immutable field set the guest owns.
    ResponseHeaders(ResourceHandle),
    /// The response body, once.
    ResponseConsume(ResourceHandle),
    /// The body's input stream, once.
    IncomingBodyStream(ResourceHandle),
    /// End the body; the trailers future the host answers empty.
    IncomingBodyFinish(ResourceHandle),
}

/// The complete result of a `wasi:http` operation.
#[derive(Debug)]
pub enum Response {
    /// The operation completed without a value.
    Done,
    /// A newly allocated resource.
    Handle(ResourceHandle),
    /// A yes or no.
    Bool(bool),
    /// The values of one header name.
    Values(Vec<Vec<u8>>),
    /// Every header entry.
    Entries(Vec<Field>),
    /// A header operation refused.
    HeaderError(HeaderError),
    /// A request operation refused (canonical `result<_, _>` err).
    Refused,
    /// The response status code.
    Status(u16),
    /// The response has not arrived.
    Pending,
    /// The response was already taken.
    Taken,
    /// The request failed with a `wasi:http` error code.
    Failed(ErrorCode),
}
