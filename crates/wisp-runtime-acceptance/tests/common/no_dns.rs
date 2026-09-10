//! The resolver of a test with no name to resolve.

use wisp_host::egress::DnsError;
use wisp_host::Resolver;

/// A resolver a test with no name to resolve gives the reactor: reaching it
/// is the failure.
pub struct NoDns;

#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<std::net::SocketAddr>, DnsError> {
        panic!("the guest must not resolve {host}: this behaviour needs no host lookup")
    }
}
