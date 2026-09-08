//! Socket creation is a deployment responsibility; the local provider uses native TCP.

use std::net::SocketAddr;
use tokio::net::TcpStream;

/// Host-established attribution for an outbound connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOwner {
    tenant: String,
    instance: crate::InstanceId,
}

impl ConnectionOwner {
    /// Establish connection attribution, refusing an absent tenant identity.
    pub fn new(tenant: String, instance: crate::InstanceId) -> std::io::Result<Self> {
        if tenant.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "outbound connection has no tenant identity",
            ));
        }
        Ok(Self { tenant, instance })
    }

    /// Tenant established by the host's dispatch routing.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Instance that owns this connection and its pending I/O.
    pub fn instance(&self) -> crate::InstanceId {
        self.instance
    }
}

/// A policy refusal, transport failure, or incorrectly bound connection.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// Destination policy refused the operation.
    #[error("{0}")]
    Denied(#[from] super::DenyReason),
    /// The connection provider or operating system failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The provider returned a connection to an unchecked peer.
    #[error("connection factory opened {actual} instead of checked destination {expected}")]
    PeerMismatch {
        /// Endpoint admitted by policy.
        expected: SocketAddr,
        /// Endpoint actually connected by the provider.
        actual: SocketAddr,
    },
}

impl From<ConnectionError> for wisp_abi::native_io::Error {
    fn from(error: ConnectionError) -> Self {
        match error {
            ConnectionError::Denied(reason) => Self::Denied(reason.to_string()),
            ConnectionError::Io(error) => Self::from(error),
            mismatch @ ConnectionError::PeerMismatch { .. } => Self::Connection {
                kind: std::io::ErrorKind::PermissionDenied,
                message: mismatch.to_string(),
            },
        }
    }
}

/// Opens a checked destination within the deployment's networking environment.
#[async_trait::async_trait]
pub trait ConnectionFactory: Send + Sync + 'static {
    /// Open host-owned TCP for this identity; dropping the future cancels the attempt.
    async fn connect(
        &self,
        owner: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<TcpStream>;
}

/// Native local networking for the open-source node and developer runtime.
#[derive(Default)]
pub struct LocalConnections;

#[async_trait::async_trait]
impl ConnectionFactory for LocalConnections {
    async fn connect(
        &self,
        owner: &ConnectionOwner,
        destination: SocketAddr,
    ) -> std::io::Result<TcpStream> {
        tracing::debug!(tenant = owner.tenant(), instance = %owner.instance(), %destination, "opening local outbound connection");
        TcpStream::connect(destination).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_cannot_default_an_absent_tenant_identity() {
        let error = ConnectionOwner::new(String::new(), crate::InstanceId(1))
            .expect_err("an unidentified connection must be refused");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidInput,
            "the factory must receive an established tenant instead of an anonymous default"
        );
    }
}
