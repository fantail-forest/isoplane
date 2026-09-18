//! One local request through a fresh debug guest and the ordinary host services.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use isoplane_host::egress::{DenyReason, DestinationPolicy, PublicDestinations};

/// The one session-owned debugger destination allowed alongside public egress.
#[derive(Clone, Copy)]
pub struct DebugPeer(SocketAddr);

impl DebugPeer {
    /// Refuses non-loopback peers and unbound port zero.
    pub fn new(address: SocketAddr) -> Result<Self, super::Error> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
            return Err(super::Error::DebugEndpoint);
        }
        Ok(Self(address))
    }
}

impl DestinationPolicy for DebugPeer {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address == self.0 {
            Ok(())
        } else {
            PublicDestinations.authorize(address)
        }
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if address == self.0.ip() {
            Ok(())
        } else {
            PublicDestinations.authorize_resolution(address)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_debug_peer_bypasses_private_egress_rejection() {
        let endpoint: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let policy = DebugPeer::new(endpoint).unwrap();
        assert!(
            policy.authorize(endpoint).is_ok(),
            "the owned debugger peer must be reachable"
        );
        assert!(
            policy.authorize_resolution(endpoint.ip()).is_ok(),
            "the owned peer IP must resolve before its exact port is checked"
        );
        for rejected in [
            "127.0.0.1:12346",
            "127.0.0.2:12345",
            "10.0.0.1:12345",
            "[::1]:12345",
        ] {
            assert!(
                policy.authorize(rejected.parse().unwrap()).is_err(),
                "a debugger exception must not grant private egress to {rejected}"
            );
        }
        assert!(
            policy
                .authorize_resolution("10.0.0.1".parse().unwrap())
                .is_err(),
            "debugging must not broaden private DNS answers"
        );
        assert!(
            policy.authorize("8.8.8.8:443".parse().unwrap()).is_ok(),
            "ordinary public egress must remain available"
        );
    }

    #[test]
    fn debugger_peer_configuration_requires_a_bound_ipv4_loopback_endpoint() {
        for rejected in [
            "127.0.0.1:0",
            "0.0.0.0:12345",
            "8.8.8.8:12345",
            "[::1]:12345",
        ] {
            assert!(
                DebugPeer::new(rejected.parse().unwrap()).is_err(),
                "invalid debugger endpoint {rejected} must fail before listening"
            );
        }
    }
}
