//! The resolver that answers every name the same way.

use std::net::SocketAddr;

use wisp_host::egress::DnsError;
use wisp_host::Resolver;

/// A resolver answering every name with the same addresses.
pub struct FixedDns(pub Vec<std::net::IpAddr>);

#[async_trait::async_trait]
impl Resolver for FixedDns {
    async fn resolve(&self, _: &str, port: u16) -> Result<Vec<SocketAddr>, DnsError> {
        Ok(self.0.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
    }
}
