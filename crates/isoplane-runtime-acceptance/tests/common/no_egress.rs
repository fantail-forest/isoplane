//! A reactor for behaviour that reaches neither the network nor a resolver.

use isoplane_harness::Reactor;
use isoplane_host::InstanceId;

use crate::no_dns::NoDns;

/// A reactor whose guest reaches neither the network nor a resolver.
pub fn reactor_without_egress(owner: u64) -> Reactor {
    Reactor::with_network(
        InstanceId(owner),
        isoplane_harness::Network::with_resolver(Box::new(NoDns)),
    )
}
