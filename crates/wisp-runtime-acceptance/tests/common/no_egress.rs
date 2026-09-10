//! A reactor for behaviour that reaches neither the network nor a resolver.

use wisp_harness::Reactor;
use wisp_host::egress::{DnsError, HostEgressPolicy};
use wisp_host::{InstanceId, Resolver};

/// A resolver a test with no name to resolve gives the reactor: reaching it
/// is the failure.
pub struct NoDns;

#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<std::net::SocketAddr>, DnsError> {
        panic!("the guest must not resolve {host}: this behaviour needs no host lookup")
    }
}

/// A reactor whose guest reaches neither the network nor a resolver.
pub fn reactor_without_egress(owner: u64) -> Reactor {
    Reactor::new(
        InstanceId(owner),
        HostEgressPolicy::new(Box::new(NoDns), 8, 1 << 20),
    )
}
