use std::sync::Arc;

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};

use wisp_host::cell::{Cell, CellConfig, Services};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::FakeFetcher;
use wisp_host::Resolver;

pub struct Fixed(pub Vec<std::net::SocketAddr>);
#[async_trait::async_trait]
impl Resolver for Fixed {
    async fn resolve(
        &self,
        _h: &str,
        _p: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
        Ok(self.0.clone())
    }
}

/// A cell with a controlled fetcher and an egress resolver fixed to `addr`.
pub fn rig_resolving(cfg: CellConfig, addr: &str) -> (Arc<Cell>, Arc<FakeFetcher>) {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        Box::new(Fixed(vec![addr.parse().expect("a literal address")])),
        1024,
        1 << 30,
    ));
    let svc = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    let cell = Cell::new(cfg, svc).expect("a cell starts");
    (cell, fetcher)
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
