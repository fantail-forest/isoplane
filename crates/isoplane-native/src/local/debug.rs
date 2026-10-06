//! One local request through a fresh debug guest and the ordinary host services.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Refuses a debugger endpoint that is not a bound IPv4 loopback port: the
/// guest dials whatever address the session names for its debug agent.
pub fn require_loopback_endpoint(address: SocketAddr) -> Result<(), super::Error> {
    if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
        return Err(super::Error::DebugEndpoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugger_peer_configuration_requires_a_bound_ipv4_loopback_endpoint() {
        for rejected in [
            "127.0.0.1:0",
            "0.0.0.0:12345",
            "8.8.8.8:12345",
            "[::1]:12345",
        ] {
            assert!(
                require_loopback_endpoint(rejected.parse().unwrap()).is_err(),
                "invalid debugger endpoint {rejected} must fail before listening"
            );
        }
    }
}
