//! Initialization's serializable outbound authority and bounded reactor.

use isoplane_host::bindings::{PreparationIo, PreparationIoConfig};
use isoplane_host::egress::{
    AdditionalDestinations, ConnectionCeilings, ConnectionLimits, ConnectionOwner,
    DestinationPolicy, HostEgressPolicy, LocalDevelopmentDestinations, PublicDestinations,
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
    /// The destinations initialization may reach, as execution will.
    pub destinations: InitializationDestinations,
    /// Connection and byte limits shared with application execution.
    pub limits: isoplane_runtime::network::NetworkLimits,
    /// Maximum retained native handles.
    pub native_handles: usize,
    /// Maximum pending native futures.
    pub pending_futures: u32,
}

/// The destination policy initialization shares with the application's execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InitializationDestinations {
    /// The node's public egress, with exact operator-approved exceptions.
    Public { exceptions: Vec<SocketAddr> },
    /// A local development session, which restricts no destination.
    LocalDevelopment,
}

impl InitializationDestinations {
    fn policy(&self) -> Arc<dyn DestinationPolicy> {
        match self {
            Self::Public { exceptions } => Arc::new(AdditionalDestinations {
                endpoints: exceptions.clone(),
                baseline: Arc::new(PublicDestinations),
            }),
            Self::LocalDevelopment => Arc::new(LocalDevelopmentDestinations),
        }
    }
}

impl InitializationNetwork {
    pub(crate) fn start(
        &self,
        reactor: tokio::runtime::Handle,
    ) -> Result<PreparationIo, crate::PrepareError> {
        let owner = ConnectionOwner::new(self.tenant.clone(), isoplane_host::InstanceId(1))
            .map_err(|error| crate::PrepareError::State(error.into()))?;
        let limits = ConnectionLimits::new(ConnectionCeilings {
            node: self.limits.connections,
            tenant: self.limits.tenant_connections,
            instance_pending: self.limits.instance_pending_connections,
        })
        .map_err(|error| crate::PrepareError::State(error.into()))?;
        let egress = HostEgressPolicy::system(limits)
            .map_err(|error| crate::PrepareError::State(error.into()))?
            .with_destinations(self.destinations.policy());
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
