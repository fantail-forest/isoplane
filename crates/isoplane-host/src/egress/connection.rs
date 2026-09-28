//! Socket creation is a deployment responsibility; the local provider uses native TCP.

use std::net::SocketAddr;
use tokio::net::TcpStream;

/// Host-established attribution for an outbound connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionOwner {
    tenant: String,
    instance: crate::InstanceId,
    exceptions: std::sync::Arc<[SocketAddr]>,
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
        Ok(Self {
            tenant,
            instance,
            exceptions: std::sync::Arc::new([]),
        })
    }

    /// Lets this owner reach `exceptions` where the node's destination policy refuses them.
    pub fn with_exceptions(mut self, exceptions: std::sync::Arc<[SocketAddr]>) -> Self {
        self.exceptions = exceptions;
        self
    }

    /// Exact peers this owner may reach where the node's destination policy refuses them.
    pub fn exceptions(&self) -> &[SocketAddr] {
        &self.exceptions
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

impl From<ConnectionError> for isoplane_abi::native_io::Error {
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

/// Native local networking for the local node and developer runtime.
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
        TcpStream::connect(canonical_destination(destination)?).await
    }
}

pub(super) fn canonical_destination(address: SocketAddr) -> std::io::Result<SocketAddr> {
    if let SocketAddr::V6(ipv6) = address {
        if let Some(ipv4) = ipv6.ip().to_ipv4_mapped() {
            if ipv6.scope_id() != 0 || ipv6.flowinfo() != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "an IPv4-mapped destination cannot carry IPv6 scope or flow information",
                ));
            }
            return Ok(SocketAddr::new(ipv4.into(), ipv6.port()));
        }
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OwnedPeer(SocketAddr);

    impl crate::egress::DestinationPolicy for OwnedPeer {
        fn authorize(&self, address: SocketAddr) -> Result<(), crate::egress::DenyReason> {
            if address.ip().to_canonical() == self.0.ip().to_canonical()
                && address.port() == self.0.port()
            {
                Ok(())
            } else {
                Err(crate::egress::DenyReason::NotAllowlisted(
                    address.to_string(),
                ))
            }
        }
    }

    async fn ipv4_round_trip(mapped: bool, hostname: bool) {
        use crate::egress::{HostEgressPolicy, Resolver, SystemResolver};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let resolved = if hostname {
            SystemResolver::new()
                .unwrap()
                .resolve("localhost", address.port())
                .await
                .unwrap()
                .into_iter()
                .find(|address| address.is_ipv4())
                .expect("localhost must identify its IPv4 loopback peer")
        } else {
            address
        };
        let destination = if mapped {
            let std::net::IpAddr::V4(ip) = resolved.ip() else {
                panic!("the fixture selected an IPv4 peer")
            };
            SocketAddr::new(ip.to_ipv6_mapped().into(), resolved.port())
        } else {
            resolved
        };
        let policy = HostEgressPolicy::system(1)
            .unwrap()
            .with_destinations(std::sync::Arc::new(OwnedPeer(address)));
        let owner =
            ConnectionOwner::new("ipv4-qualification".into(), crate::InstanceId(1)).unwrap();
        let mut stream = policy.connect(&owner, destination).await.unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        stream.write_all(b"ipv4").await.unwrap();
        let mut received = [0; 4];
        peer.read_exact(&mut received).await.unwrap();
        assert_eq!(
            received, *b"ipv4",
            "the checked provider must deliver bytes to the owned IPv4 peer"
        );
        assert!(
            stream.peer_addr().unwrap().is_ipv4(),
            "an IPv4 destination must use native IPv4 without requiring IPv6 support"
        );
    }

    #[tokio::test]
    async fn a_numeric_ipv4_peer_uses_native_ipv4() {
        ipv4_round_trip(false, false).await;
    }

    #[tokio::test]
    async fn a_mapped_numeric_ipv4_peer_does_not_require_ipv6() {
        ipv4_round_trip(true, false).await;
    }

    #[tokio::test]
    async fn a_mapped_hostname_ipv4_peer_does_not_require_ipv6() {
        ipv4_round_trip(true, true).await;
    }

    #[tokio::test]
    async fn an_owners_exception_admits_exactly_its_private_peer() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let peer = listener.local_addr().unwrap();
        let policy = crate::egress::HostEgressPolicy::system(2).unwrap();
        let stranger = ConnectionOwner::new("stranger".into(), crate::InstanceId(1)).unwrap();
        assert!(
            matches!(
                policy.connect(&stranger, peer).await,
                Err(ConnectionError::Denied(_))
            ),
            "the public policy must refuse a loopback peer to an owner without an exception"
        );
        let excepted = ConnectionOwner::new("excepted".into(), crate::InstanceId(2))
            .unwrap()
            .with_exceptions(std::sync::Arc::new([peer]));
        assert!(
            policy.connect(&excepted, peer).await.is_ok(),
            "an owner's exception must admit its exact peer"
        );
        let neighbour = SocketAddr::new(peer.ip(), peer.port().wrapping_add(1));
        assert!(
            matches!(
                policy.connect(&excepted, neighbour).await,
                Err(ConnectionError::Denied(_))
            ),
            "an exception must not admit another port on the same address"
        );
    }

    #[test]
    fn native_ipv6_destination_metadata_is_preserved() {
        let address = SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::7".parse().unwrap(),
            443,
            17,
            4,
        ));
        assert_eq!(
            canonical_destination(address).unwrap(),
            address,
            "native IPv6 must retain its scope, flow information, address and port"
        );
    }

    #[test]
    fn mapped_ipv4_scope_and_flow_information_are_refused() {
        for (flow, scope) in [(1, 0), (0, 1)] {
            let address = SocketAddr::V6(std::net::SocketAddrV6::new(
                std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
                443,
                flow,
                scope,
            ));
            let error =
                canonical_destination(address).expect_err("IPv4 cannot preserve IPv6 metadata");
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "mapped metadata must not be silently discarded"
            );
        }
    }

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
