//! Explicit endpoint exceptions composed with a host-selected destination policy.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use super::{DenyReason, DestinationPolicy};

/// Supplements a destination policy with exact operator-approved endpoints.
/// Resolution may expose each endpoint's IP; connection authorization still checks its port.
pub struct AdditionalDestinations {
    /// Exact additional peers that guests may connect to.
    pub endpoints: Vec<SocketAddr>,
    /// Policy used for every destination outside the explicit exceptions.
    pub baseline: Arc<dyn DestinationPolicy>,
}

impl DestinationPolicy for AdditionalDestinations {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if self.endpoints.contains(&address) {
            Ok(())
        } else {
            self.baseline.authorize(address)
        }
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if self
            .endpoints
            .iter()
            .any(|endpoint| endpoint.ip() == address)
        {
            Ok(())
        } else {
            self.baseline.authorize_resolution(address)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_exceptions_preserve_a_custom_baseline_and_do_not_allow_neighboring_ports() {
        let database: SocketAddr = "127.0.0.1:5432".parse().unwrap();
        let debugger: SocketAddr = "127.0.0.2:5000".parse().unwrap();
        let policy = AdditionalDestinations {
            endpoints: vec![database],
            baseline: Arc::new(AdditionalDestinations {
                endpoints: vec![debugger],
                baseline: Arc::new(super::super::PublicDestinations),
            }),
        };
        for allowed in [database, debugger, "1.1.1.1:443".parse().unwrap()] {
            assert!(
                policy.authorize(allowed).is_ok(),
                "composed authority must retain {allowed}"
            );
            assert!(
                policy.authorize_resolution(allowed.ip()).is_ok(),
                "resolution must preserve the composed authority for {allowed}"
            );
        }
        for denied in ["127.0.0.1:5433", "127.0.0.2:5001", "127.0.0.3:5000"] {
            assert!(
                policy.authorize(denied.parse().unwrap()).is_err(),
                "an endpoint exception must not authorize {denied}"
            );
        }
        assert!(
            policy
                .authorize_resolution("127.0.0.3".parse().unwrap())
                .is_err(),
            "unapproved private IP addresses must remain hidden during resolution"
        );
    }
}
