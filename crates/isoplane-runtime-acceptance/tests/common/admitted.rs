//! A destination policy admitting exactly the peers a test owns.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use isoplane_harness::Reactor;
use isoplane_host::egress::{DenyReason, DestinationPolicy};
use isoplane_host::Resolver;

/// A destination policy admitting exactly the test's own peers.
struct OnlyPeers(Vec<SocketAddr>);

impl DestinationPolicy for OnlyPeers {
    fn authorize(&self, peer: SocketAddr) -> Result<(), DenyReason> {
        if self.0.contains(&peer) {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(peer.to_string()))
        }
    }

    /// A name may resolve to an admitted peer's address: the peers live on
    /// the loopback, which the production policy would refuse.
    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if self.0.iter().any(|peer| peer.ip() == address) {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}

/// An egress policy whose outbound connections may only reach `peers`,
/// resolving names through `resolver`.
pub fn admitting(
    mut peers: Vec<SocketAddr>,
    resolver: Box<dyn Resolver>,
) -> isoplane_harness::Network {
    let aliases: Vec<_> = peers
        .iter()
        .filter_map(|peer| match peer {
            SocketAddr::V4(address) => Some(SocketAddr::new(
                IpAddr::V6(address.ip().to_ipv6_mapped()),
                address.port(),
            )),
            SocketAddr::V6(address) if address.scope_id() == 0 && address.flowinfo() == 0 => {
                address
                    .ip()
                    .to_ipv4_mapped()
                    .map(|ip| SocketAddr::new(IpAddr::V4(ip), address.port()))
            }
            SocketAddr::V6(_) => None,
        })
        .collect();
    peers.extend(aliases);
    isoplane_harness::Network::with_resolver(resolver).with_destinations(Arc::new(OnlyPeers(peers)))
}

/// Waits for the reactor to release every connection permit: a cancelled
/// operation's readiness task is dropped on the scheduler's next turn, so a
/// test that just cancelled one yields to it, failing rather than hanging
/// past the deadline.
pub async fn released(reactor: &Reactor) {
    tokio::time::timeout(isoplane_harness::WAKE_DEADLINE, async {
        while reactor.runtime().services.egress.connections() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the reactor must release the cancelled operation's connection");
}
