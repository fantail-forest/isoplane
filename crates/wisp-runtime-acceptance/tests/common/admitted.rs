//! A destination policy admitting exactly the peers a test owns.

use std::net::SocketAddr;
use std::sync::Arc;

use wisp_harness::Reactor;
use wisp_host::egress::{DenyReason, DestinationPolicy, HostEgressPolicy};
use wisp_host::Resolver;

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
}

/// An egress policy whose outbound connections may only reach `peers`,
/// resolving names through `resolver`.
pub fn admitting(peers: Vec<SocketAddr>, resolver: Box<dyn Resolver>) -> HostEgressPolicy {
    HostEgressPolicy::new(resolver, 8, 1 << 20).with_destinations(Arc::new(OnlyPeers(peers)))
}

/// Waits for the reactor to release every connection permit: a cancelled
/// operation's readiness task is dropped on the scheduler's next turn, so a
/// test that just cancelled one yields to it, failing rather than hanging
/// past the deadline.
pub async fn released(reactor: &Reactor) {
    tokio::time::timeout(wisp_harness::WAKE_DEADLINE, async {
        while reactor.runtime().services.egress.connections() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the reactor must release the cancelled operation's connection");
}
