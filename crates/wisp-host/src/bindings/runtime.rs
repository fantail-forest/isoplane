//! The per-cell machinery every instance's bindings share: the services the
//! vtable routes to, the future table, and the wake path back to a pool
//! thread.

use std::sync::Arc;

use wisp_abi::calls::FetchRequest;
use wisp_abi::result::FutureError;

use crate::egress::HostEgressPolicy;
use crate::futures::{FutureTable, WakeTarget};
use crate::metrics::Metrics;
use crate::InstanceId;

/// How a completed host future gets back to an execution-pool thread.
pub trait WakeSink: Send + Sync + 'static {
    fn signal(&self, owner: InstanceId);
}

/// Sends an outbound request and transfers ownership of its response body.
#[async_trait::async_trait]
pub trait Fetcher: Send + Sync + 'static {
    /// `target.addr` is the pinned address from the egress check; an
    /// implementation that re-resolves `target.url` has thrown the rebinding
    /// control away.
    async fn fetch(
        &self,
        owner: crate::egress::ConnectionOwner,
        target: wisp_abi::host::PinnedTarget,
        req: FetchRequest,
    ) -> Result<super::http_scope::OutboundResponse, FutureError>;
}

/// Everything the vtable routes to.
pub struct Services {
    pub egress: Arc<HostEgressPolicy>,
    pub fetcher: Arc<dyn Fetcher>,
}

/// The per-cell machinery an instance's bindings share; per-instance state
/// lives in [`InstanceCtx`].
pub struct CellRuntime {
    pub services: Arc<Services>,
    pub futures: Arc<FutureTable>,
    pub metrics: Arc<Metrics>,
    pub wake: Arc<dyn WakeSink>,
    /// Reactor threads own all I/O; an execution thread never awaits.
    pub reactor: tokio::runtime::Handle,
}

impl CellRuntime {
    /// The one completion path, for the vtable's own futures and for
    /// `Cell::complete_future`.
    pub fn settle_future(&self, id: u64, res: wisp_abi::result::ResultRecord) {
        let WakeTarget::Instance(owner) = self.futures.complete(id, res) else {
            return;
        };
        self.wake.signal(owner);
    }
}
