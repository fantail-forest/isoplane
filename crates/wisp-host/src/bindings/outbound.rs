//! What the outbound HTTP transport exchanges on a guest's behalf: a request
//! whose body the guest streams as it writes it, and the ways an exchange
//! fails before a response head arrives, typed so the adapter can name them
//! to the guest.

use bytes::Bytes;
use tokio::sync::mpsc;

/// One piece of a streamed request body.
#[derive(Debug)]
pub enum BodyFrame {
    /// Bytes the guest wrote.
    Data(Bytes),
    /// The guest finished the body; nothing follows.
    End,
}

/// The body of an outbound request.
pub enum RequestBody {
    /// No body.
    Empty,
    /// Frames the guest writes while the request is in flight; a channel
    /// that closes without [`BodyFrame::End`] aborts the request.
    Stream(mpsc::Receiver<BodyFrame>),
}

/// An outbound request as the guest built it.
pub struct OutboundRequest {
    /// The HTTP method token.
    pub method: String,
    /// The absolute URL; the transport connects to the pinned address and
    /// uses the URL for the request line, the Host header and TLS.
    pub url: String,
    /// Header names and values, in order.
    pub headers: Vec<(String, Vec<u8>)>,
    /// The body.
    pub body: RequestBody,
}

/// Why TLS did not come up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsFailure {
    /// The server's certificate is not trusted or does not name the host.
    Certificate,
    /// The server sent an alert.
    Alert,
    /// Any other handshake failure.
    Protocol,
}

/// An exchange that produced no response head.
#[derive(Debug)]
pub enum ExchangeFailure {
    /// The URL is not one the transport sends.
    Uri(String),
    /// The connection could not be made.
    Connect(crate::egress::ConnectionError),
    /// TLS failed.
    Tls(TlsFailure),
    /// The peer violated HTTP or closed before answering.
    Protocol(String),
    /// The transport itself failed.
    Internal(String),
}

impl std::fmt::Display for ExchangeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExchangeFailure::Uri(reason) => write!(f, "request URL refused: {reason}"),
            ExchangeFailure::Connect(error) => write!(f, "connect failed: {error}"),
            ExchangeFailure::Tls(TlsFailure::Certificate) => {
                write!(f, "TLS: the server certificate is not trusted")
            }
            ExchangeFailure::Tls(TlsFailure::Alert) => write!(f, "TLS: the server sent an alert"),
            ExchangeFailure::Tls(TlsFailure::Protocol) => write!(f, "TLS handshake failed"),
            ExchangeFailure::Protocol(reason) => write!(f, "HTTP exchange failed: {reason}"),
            ExchangeFailure::Internal(reason) => write!(f, "outbound HTTP failed: {reason}"),
        }
    }
}

impl std::error::Error for ExchangeFailure {}
