use std::net::SocketAddr;
use wisp_host::egress::{DenyReason, DestinationPolicy};

pub struct OwnedEndpoint(pub SocketAddr);
impl DestinationPolicy for OwnedEndpoint {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address.port() == self.0.port()
            && address.ip().to_canonical() == self.0.ip().to_canonical()
        {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owned_endpoint_accepts_only_its_exact_ipv4_or_mapped_equivalent() {
        let policy = OwnedEndpoint("127.0.0.1:1234".parse().expect("valid owned endpoint"));
        for endpoint in ["127.0.0.1:1234", "[::ffff:127.0.0.1]:1234"] {
            assert!(
                policy
                    .authorize(endpoint.parse().expect("valid endpoint"))
                    .is_ok(),
                "the owned endpoint must accept its exact mapped equivalent: {endpoint}"
            );
        }
        for endpoint in [
            "127.0.0.2:1234",
            "[::ffff:127.0.0.2]:1234",
            "127.0.0.1:1235",
            "[::1]:1234",
        ] {
            assert!(
                policy
                    .authorize(endpoint.parse().expect("valid endpoint"))
                    .is_err(),
                "the owned endpoint must refuse another address or port: {endpoint}"
            );
        }
    }
}
