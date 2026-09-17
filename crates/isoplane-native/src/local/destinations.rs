//! Explicit development services supplement the production public-egress policy.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use wisp_host::egress::{
    AdditionalDestinations, DenyReason, DestinationPolicy, PublicDestinations,
};

pub(super) struct DevelopmentDestinations(AdditionalDestinations);

impl DevelopmentDestinations {
    /// Composes approved service endpoints with the optional session debugger.
    pub fn new(endpoints: Vec<SocketAddr>, debugger: Option<super::debug::DebugPeer>) -> Self {
        let baseline: Arc<dyn DestinationPolicy> = match debugger {
            Some(peer) => Arc::new(peer),
            None => Arc::new(PublicDestinations),
        };
        Self(AdditionalDestinations {
            endpoints,
            baseline,
        })
    }
}

impl DestinationPolicy for DevelopmentDestinations {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        let result = self.0.authorize(address);
        if result.is_err() {
            tracing::debug!(destination = %address, allowed_endpoints = ?self.0.endpoints, "local development destination refused");
        }
        result
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        self.0.authorize_resolution(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_debug_session_retains_both_its_peer_and_explicit_service_endpoints() {
        let database = "127.0.0.1:5432".parse().unwrap();
        let debugger = "127.0.0.1:5000".parse().unwrap();
        let peer = super::super::debug::DebugPeer::new(debugger).unwrap();
        let policy = DevelopmentDestinations::new(vec![database], Some(peer));
        for allowed in [database, debugger, "1.1.1.1:443".parse().unwrap()] {
            assert!(
                policy.authorize(allowed).is_ok(),
                "local debugging must preserve authorization for {allowed}"
            );
            assert!(
                policy.authorize_resolution(allowed.ip()).is_ok(),
                "local debugging must preserve resolution for {allowed}"
            );
        }
        assert!(
            policy.authorize("127.0.0.1:5001".parse().unwrap()).is_err(),
            "the debugger exception must not grant a neighboring port"
        );
        assert!(
            policy
                .authorize_resolution("127.0.0.2".parse().unwrap())
                .is_err(),
            "the debugger exception must not grant another loopback IP"
        );
    }

    #[test]
    fn a_local_service_exception_does_not_grant_other_private_endpoints() {
        let database = "127.0.0.1:5432".parse().unwrap();
        let destinations = DevelopmentDestinations::new(vec![database], None);
        assert!(
            destinations.authorize(database).is_ok(),
            "the configured development database must be reachable"
        );
        for other in [
            "127.0.0.1:5433",
            "127.0.0.2:5432",
            "10.0.0.1:5432",
            "[::1]:5432",
        ] {
            assert!(
                destinations.authorize(other.parse().unwrap()).is_err(),
                "a local database exception must not authorize {other}"
            );
        }
        assert!(
            destinations.authorize_resolution(database.ip()).is_ok(),
            "the database's name may resolve to its allowed address"
        );
        assert!(
            destinations
                .authorize_resolution("10.0.0.1".parse().unwrap())
                .is_err(),
            "unconfigured private resolution must remain denied"
        );
    }
}
