use std::sync::Arc;

use isoplane_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};

use isoplane_host::cell::{Cell, CellConfig, Services};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fakes::FakeFetcher;
use isoplane_host::Resolver;

pub struct Fixed(pub Vec<std::net::SocketAddr>);
#[async_trait::async_trait]
impl Resolver for Fixed {
    async fn resolve(
        &self,
        _h: &str,
        _p: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        Ok(self.0.clone())
    }
}

/// A cell with a controlled fetcher and an egress resolver fixed to `addr`.
#[allow(
    dead_code,
    reason = "Suites supplying snapshot storage use services_resolving directly"
)]
pub fn rig_resolving(cfg: CellConfig, addr: &str) -> (Arc<Cell>, Arc<FakeFetcher>) {
    let (svc, fetcher) = services_resolving(addr);
    let cell = Cell::new(cfg, svc).expect("a cell starts");
    (cell, fetcher)
}

pub fn services_resolving(addr: &str) -> (Arc<Services>, Arc<FakeFetcher>) {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        Box::new(Fixed(vec![addr.parse().expect("a literal address")])),
        isoplane_host::egress::ConnectionLimits::uniform(1024).expect("connection limits"),
    ));
    let svc = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    (svc, fetcher)
}

pub fn manifest(name: &str, lifecycle: HandlerLifecycle, limits: Limits) -> AppManifest {
    AppManifest {
        guest_memory_bytes: None,
        resource_limits: Default::default(),
        validation_limits: Default::default(),
        name: name.into(),
        handlers: HandlerConfig { lifecycle },
        bindings: Bindings::default(),
        limits: limits,
        init_visible_vars: vec![],
    }
}
