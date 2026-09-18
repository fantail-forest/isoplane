//! Initialization's serializable outbound authority and bounded reactor.

use isoplane_host::bindings::{PreparationIo, PreparationIoConfig};
use isoplane_host::egress::{
    AdditionalDestinations, ConnectionOwner, HostEgressPolicy, PublicDestinations,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

/// Application outbound policy copied from its execution configuration.
#[derive(Clone, Serialize, Deserialize)]
pub struct InitializationNetwork {
    /// Established tenant identity for egress attribution.
    pub tenant: String,
    /// Application restrictions intersected with platform destination policy.
    pub allowlist: Option<Vec<String>>,
    /// Operator-approved local development endpoints; empty in production.
    pub allowed_destinations: Vec<SocketAddr>,
    /// Connection and byte limits shared with application execution.
    pub limits: isoplane_runtime::network::NetworkLimits,
    /// Maximum retained native handles.
    pub native_handles: usize,
    /// Maximum pending native futures.
    pub pending_futures: u32,
}

impl InitializationNetwork {
    pub(crate) fn start(
        &self,
        reactor: tokio::runtime::Handle,
    ) -> Result<PreparationIo, crate::PrepareError> {
        let owner = ConnectionOwner::new(self.tenant.clone(), isoplane_host::InstanceId(1))
            .map_err(|error| crate::PrepareError::State(error.into()))?;
        let egress = HostEgressPolicy::system(self.limits.connections, self.limits.egress_bytes)
            .map_err(|error| crate::PrepareError::State(error.into()))?
            .with_destinations(Arc::new(AdditionalDestinations {
                endpoints: self.allowed_destinations.clone(),
                baseline: Arc::new(PublicDestinations),
            }));
        Ok(PreparationIo::new(
            PreparationIoConfig {
                owner,
                egress: Arc::new(egress),
                allowlist: self.allowlist.clone(),
                pending_futures: self.pending_futures,
                native_handles: self.native_handles,
            },
            reactor,
        ))
    }
}
