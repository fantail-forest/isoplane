//! Explicit development services supplement the production public-egress policy.

use std::net::{IpAddr, SocketAddr};
use wisp_host::egress::{DenyReason, DestinationPolicy, PublicDestinations};

pub(super) struct DevelopmentDestinations {
    pub endpoints: Vec<SocketAddr>,
    pub debugger: Option<super::debug::DebugPeer>,
}

impl DestinationPolicy for DevelopmentDestinations {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if self.endpoints.contains(&address) {
            return Ok(());
        }
        let result = match &self.debugger {
            Some(peer) => peer.authorize(address),
            None => PublicDestinations.authorize(address),
        };
        if result.is_err() {
            tracing::debug!(destination = %address, allowed_endpoints = ?self.endpoints, "local development destination refused");
        }
        result
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if self
            .endpoints
            .iter()
            .any(|endpoint| endpoint.ip() == address)
        {
            return Ok(());
        }
        match &self.debugger {
            Some(peer) => peer.authorize_resolution(address),
            None => PublicDestinations.authorize_resolution(address),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_service_exception_does_not_grant_other_private_endpoints() {
        let database = "127.0.0.1:5432".parse().unwrap();
        let destinations = DevelopmentDestinations {
            endpoints: vec![database],
            debugger: None,
        };
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
