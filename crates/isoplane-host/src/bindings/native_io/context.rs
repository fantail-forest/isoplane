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
}

impl Runtime {
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
    fn reach(&self) -> crate::egress::Reach<'_>;
    fn connection_owner(&self) -> std::io::Result<crate::egress::ConnectionOwner>;
    fn alloc(&self, import: &'static str) -> Option<u64>;
}

impl Context for CellBindings {
    fn runtime(&self) -> Option<Runtime> {
        self.rt().map(|runtime| Runtime {
            egress: runtime.services.egress.clone(),
            futures: runtime.futures.clone(),
            metrics: runtime.metrics.clone(),
            wake: runtime.wake.clone(),
            reactor: runtime.reactor.clone(),
        })
    }
    fn tenant(&self) -> &str {
        &self.ctx.tenant
    }
    fn instance(&self) -> InstanceId {
        self.ctx.instance
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
        self.alloc(import)
    }
}
