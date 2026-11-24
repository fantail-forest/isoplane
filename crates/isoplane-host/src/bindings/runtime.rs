use std::sync::Arc;

use crate::egress::HostEgressPolicy;
use crate::futures::{FutureTable, WakeTarget};
use crate::metrics::Metrics;
use crate::InstanceId;

/// How a completed host future gets back to an execution-pool thread.
pub trait WakeSink: Send + Sync + 'static {
    fn signal(&self, owner: InstanceId);
}

/// Everything the vtable routes to.
pub struct Services {
    pub egress: Arc<HostEgressPolicy>,
    #[cfg(feature = "rust-guests")]
    pub fetcher: Arc<dyn super::Fetcher>,
}

impl Services {
    /// A node's services over `egress`. A host built for Rust guests also
    /// fetches HTTP over the real transport, returning inline the response
    /// bodies within the default threshold.
    pub fn new(egress: Arc<HostEgressPolicy>) -> Self {
        Self {
            egress,
            #[cfg(feature = "rust-guests")]
            fetcher: Arc::new(crate::PinnedHttpFetcher::new(
                isoplane_abi::config::DEFAULT_INLINE_RESPONSE_BYTES,
            )),
        }
    }

    /// Returns inline the fetched response bodies of at most `bytes`.
    #[cfg(feature = "rust-guests")]
    pub fn with_inline_response_bytes(mut self, bytes: usize) -> Self {
        self.fetcher = Arc::new(crate::PinnedHttpFetcher::new(bytes));
        self
    }
}

/// The per-cell machinery an instance's bindings share; per-instance state
/// lives in [`InstanceCtx`].
pub struct CellRuntime {
    /// Activation configuration is host-resident and never part of a template.
    pub runtime_configuration: super::RuntimeConfigurations,
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
    pub fn settle_future(&self, id: u64, res: isoplane_abi::result::ResultRecord) {
        let WakeTarget::Instance(owner) = self.futures.complete(id, res) else {
            return;
        };
        self.wake.signal(owner);
    }
}
