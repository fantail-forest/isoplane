//! One local execution pool behind a shared Host-routed listener.

pub mod deploy;

use crate::admin::NodeAdmin;
use crate::config::NodeConfig;
use isoplane_abi::host::{CodeTier, RouteTarget};
use isoplane_proxy::{LocalRouteTable, Proxy, ProxyConfig, ProxyMetrics, ShedConfig};
use std::sync::Arc;

/// One measured preparation stage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeployStage {
    /// Stage name.
    pub name: String,
    /// Observed elapsed milliseconds.
    pub ms: f64,
}

/// The shared local host; applications are installed before its listener accepts traffic.
pub struct Node {
    /// Validated operator configuration.
    pub cfg: NodeConfig,
    /// Exact Host routing published after installation succeeds.
    pub routes: Arc<LocalRouteTable>,
    /// Shared HTTP ingress and transport lifecycle.
    pub proxy: Arc<Proxy>,
    /// Read-only measurements of the local execution pool.
    pub admin: Arc<dyn NodeAdmin>,
    /// Ingress counters and bounded histograms.
    pub metrics: Arc<ProxyMetrics>,
    pub(crate) background: tokio_util::sync::CancellationToken,
    pub(crate) reclamation: Arc<crate::reclamation::Coordinator>,
    /// The real sandbox execution pool.
    pub real: Arc<crate::real::RealCells>,
    _daemon: std::fs::File,
}

impl Node {
    /// Assembles real host services without accepting traffic.
    pub fn build(cfg: NodeConfig) -> anyhow::Result<Arc<Self>> {
        Self::build_with_destinations(cfg, Arc::new(isoplane_host::egress::PublicDestinations))
    }

    /// Assembles services with a host-owned destination policy.
    pub fn build_with_destinations(
        cfg: NodeConfig,
        destinations: Arc<dyn isoplane_host::egress::DestinationPolicy>,
    ) -> anyhow::Result<Arc<Self>> {
        cfg.validate()?;
        let store = crate::installed::InstalledApplications::open(&cfg.data_dir)?;
        let daemon = store.daemon()?;
        let trust = cfg.trust_bundle()?;
        let real = crate::real::RealCells::build_with_destinations(
            &cfg,
            &crate::real::RealPaths::under(&cfg.data_dir),
            destinations,
            &trust,
        )?;
        let routes = Arc::new(LocalRouteTable::new());
        let metrics = Arc::new(ProxyMetrics::new());
        let proxy = Proxy::new(
            routes.clone(),
            real.clone(),
            metrics.clone(),
            ProxyConfig {
                listen: cfg.proxy.listen,
                shed: ShedConfig {
                    max_in_flight: cfg.proxy.max_in_flight,
                    max_queued: cfg.proxy.max_queued,
                },
            },
        );
        let admin = Arc::new(crate::real::RealNodeAdmin::new(real.clone()));
        let reclamation = crate::reclamation::Coordinator::new(&real, cfg.host.hibernate);
        let node = Arc::new(Self {
            cfg,
            routes,
            proxy,
            admin,
            metrics,
            background: tokio_util::sync::CancellationToken::new(),
            reclamation,
            real,
            _daemon: daemon,
        });
        if let Some(cell) = node.real.cells().first() {
            node.reclamation.start(
                cell.reactor(),
                std::time::Duration::from_millis(node.cfg.host.sweep_interval_ms),
                node.background.clone(),
            );
        }
        Ok(node)
    }

    /// Restores every selected application and pins its recovery files before publishing routes.
    pub async fn restore_installed(&self) -> anyhow::Result<()> {
        let store = crate::installed::InstalledApplications::open(&self.cfg.data_dir)?;
        let _startup = store.deployment()?;
        let apps: Vec<_> = self
            .cfg
            .apps
            .iter()
            .map(|app| app.manifest.name.as_str())
            .collect();
        let installed = store.load(&self.cfg.owner, &apps)?;
        let mut routes = Vec::new();
        for (app, selected) in self.cfg.apps.iter().zip(installed) {
            anyhow::ensure!(
                serde_json::to_value(&app.manifest)?
                    == serde_json::to_value(&selected.receipt.manifest)?,
                "application `{}` policy changed; redeploy its bundle before restart",
                app.manifest.name
            );
            let target = RouteTarget {
                app: app.manifest.name.clone(),
                tenant: self.cfg.owner.clone(),
                version: selected.receipt.version.clone(),
                tier: CodeTier::Optimized,
            };
            self.real
                .restore_application(&self.cfg.owner, selected)
                .await?;
            routes.push((target, &app.hosts));
        }
        for (target, hosts) in routes {
            self.routes.insert(target, hosts)?;
        }
        store.retain_configured(&self.cfg.owner, &apps)?;
        if let Err(error) = store.collect() {
            tracing::warn!(%error, "retired application cleanup failed");
        }
        Ok(())
    }

    /// Serves every application from this process and listener.
    pub async fn serve(self: Arc<Self>, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
        self.proxy.clone().serve(listener).await
    }
    /// Stops admission and cancels active transports.
    pub fn shutdown(&self) {
        self.proxy.shutdown();
        self.background.cancel();
    }
    /// Joins owned transport cleanup.
    pub async fn wait_for_shutdown(&self) -> anyhow::Result<()> {
        self.proxy.wait_for_shutdown().await;
        Ok(())
    }
}
