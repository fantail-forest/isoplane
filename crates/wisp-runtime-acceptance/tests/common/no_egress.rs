//! A reactor for behaviour that reaches neither the network nor a resolver.

use wisp_harness::Reactor;
use wisp_host::egress::HostEgressPolicy;
use wisp_host::InstanceId;

use crate::no_dns::NoDns;

/// A reactor whose guest reaches neither the network nor a resolver.
pub fn reactor_without_egress(owner: u64) -> Reactor {
    Reactor::new(
        InstanceId(owner),
        HostEgressPolicy::new(Box::new(NoDns), 8, 1 << 20),
    )
}
