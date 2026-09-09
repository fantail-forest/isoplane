use std::net::SocketAddr;
use wisp_host::egress::{DenyReason, DestinationPolicy};

pub struct OwnedEndpoint(pub SocketAddr);
impl DestinationPolicy for OwnedEndpoint {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address == self.0 {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}
