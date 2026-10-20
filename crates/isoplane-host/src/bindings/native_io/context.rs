//! Reactor authority shared by cell requests and preparation.

use std::sync::Arc;

use isoplane_abi::result::ResultRecord;

use crate::bindings::{CellBindings, WakeSink};
use crate::egress::HostEgressPolicy;
use crate::futures::{FutureTable, WakeTarget};
use crate::metrics::Metrics;
use crate::InstanceId;

#[derive(Clone)]
pub(crate) struct Runtime {
    pub egress: Arc<HostEgressPolicy>,
    pub futures: Arc<FutureTable>,
    pub metrics: Arc<Metrics>,
    pub wake: Arc<dyn WakeSink>,
    pub reactor: tokio::runtime::Handle,
    pub meters: Arc<crate::meters::Meters>,
    pub observer: Option<std::sync::Weak<dyn crate::reactor_accounting::Observer>>,
}

impl Runtime {
    pub fn spawn(
        &self,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let observer = self.observer.as_ref().and_then(std::sync::Weak::upgrade);
        let usage = crate::reactor_accounting::TaskUsage::new(self.meters.clone(), observer);
        self.reactor.spawn(usage.measure(future))
    }

    pub fn settle_future(&self, id: u64, result: ResultRecord) {
        if let WakeTarget::Instance(owner) = self.futures.complete(id, result) {
            self.wake.signal(owner);
        }
    }
}

pub(crate) trait Context {
    fn runtime(&self) -> Option<Runtime>;
    fn tenant(&self) -> &str;
    fn instance(&self) -> InstanceId;
    /// The dispatch the current slice runs; 0 outside any dispatch.
    fn dispatch(&self) -> u64;
    fn reach(&self) -> crate::egress::Reach<'_>;
    fn connection_owner(&self) -> std::io::Result<crate::egress::ConnectionOwner>;
    fn alloc(&self, import: &'static str) -> Option<u64>;
    /// A wake the host recreates in a later dispatch; see
    /// [`FutureTable::alloc_rearmable`].
    fn alloc_rearmable(&self, import: &'static str) -> Option<u64>;
}

impl Context for CellBindings {
    fn runtime(&self) -> Option<Runtime> {
        self.rt().map(|runtime| Runtime {
            egress: runtime.services.egress.clone(),
            futures: runtime.futures.clone(),
            metrics: runtime.metrics.clone(),
            wake: runtime.wake.clone(),
            reactor: runtime.reactor.clone(),
            meters: self.ctx.meters.clone(),
            observer: self.ctx.current().reactor_observer.lock().clone(),
        })
    }
    fn tenant(&self) -> &str {
        &self.ctx.tenant
    }
    fn instance(&self) -> InstanceId {
        self.ctx.instance
    }
    fn dispatch(&self) -> u64 {
        self.ctx.dispatch_id()
    }
    fn reach(&self) -> crate::egress::Reach<'_> {
        crate::egress::Reach {
            allowlist: self.ctx.allowlist.as_deref(),
            exceptions: &self.ctx.endpoint_exceptions,
        }
    }
    fn connection_owner(&self) -> std::io::Result<crate::egress::ConnectionOwner> {
        Ok(
            crate::egress::ConnectionOwner::new(self.ctx.tenant.clone(), self.ctx.instance)?
                .with_exceptions(self.ctx.endpoint_exceptions.clone()),
        )
    }
    fn alloc(&self, import: &'static str) -> Option<u64> {
        self.alloc(import, false)
    }
    fn alloc_rearmable(&self, import: &'static str) -> Option<u64> {
        self.alloc(import, true)
    }
}
