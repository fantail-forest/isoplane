//! Scripted resolvers that fail or never answer, and a reactor built
//! around one; the one that answers is [`fixed_dns`](../fixed_dns.rs).

use std::net::SocketAddr;
use std::sync::Arc;

use isoplane_harness::Reactor;
use isoplane_host::egress::{DnsError, HostEgressPolicy};
use isoplane_host::{InstanceId, Resolver};

/// A reactor whose name lookups go to `resolver`.
pub fn reactor_for_dns(owner: u64, resolver: Box<dyn Resolver>) -> Reactor {
    Reactor::new(
        InstanceId(owner),
        HostEgressPolicy::new(resolver, 8, 1 << 20),
    )
}

/// A resolver failing every name the same way.
pub struct FailingDns(pub isoplane_abi::native_io::DnsFailure);

#[async_trait::async_trait]
impl Resolver for FailingDns {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
        Err(DnsError::Lookup {
            host: host.to_owned(),
            kind: self.0,
            message: "controlled resolver failure".into(),
        })
    }
}

/// A resolver that never answers and reports when it is dropped, so a test
/// can prove the guest's cancellation released the host lookup.
pub struct PendingDns {
    pub entered: Arc<tokio::sync::Notify>,
    pub stopped: Arc<tokio::sync::Notify>,
}

struct LookupStopped(Arc<tokio::sync::Notify>);

impl Drop for LookupStopped {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait::async_trait]
impl Resolver for PendingDns {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
        let _stopped = LookupStopped(self.stopped.clone());
        self.entered.notify_one();
        std::future::pending().await
    }
}
