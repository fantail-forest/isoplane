//! Local preparation and execution cells behind the HTTP dispatcher.

mod admin;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{Dispatcher, HttpReply, RouteTarget};
use wisp_host::ModuleId;
use wisp_host::{Cell, EngineActivator};

pub use admin::RealNodeAdmin;

/// The read-only files every guest sees, at pre-initialisation and at
/// dispatch alike: today the trust bundle alone.
pub fn platform_files(
    trust: &crate::trust::TrustBundle,
) -> Result<wisp_engine::PlatformFiles, wisp_engine::PlatformFilesError> {
    wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
        path: crate::trust::GUEST_PATH.to_string(),
        bytes: trust.pem().into(),
        modified_ns: trust.modified_ns(),
    }])
}

/// The engine sizing from the node's config plus the platform files.
pub fn engine_config(
    cfg: &crate::config::NodeConfig,
    trust: &crate::trust::TrustBundle,
) -> Result<wisp_engine::EngineConfig, wisp_engine::PlatformFilesError> {
    Ok(wisp_engine::EngineConfig {
        max_instances: cfg.engine.max_instances,
        max_memory_bytes: cfg.engine.max_memory_mib << 20,
        guard_size_bytes: cfg.engine.guard_mib << 20,
        platform_files: platform_files(trust)?,
        ..wisp_engine::EngineConfig::default()
    })
}

/// The node's cells on one engine, and the placement that puts an app on
/// one of them.
pub struct RealCells {
    engine: Arc<wisp_engine::WispEngine>,
    /// Compiled template artifacts, mapped rather than read, so a restart
    /// does not recompile.
    artifacts: std::path::PathBuf,
    cells: Vec<Arc<Cell>>,
    /// Configured application-to-cell assignment.
    placement: RwLock<HashMap<String, usize>>,
    /// Exact app/version identities registered on this node.
    modules: RwLock<HashSet<ModuleId>>,
    /// app -> tenant.
    tenants: Arc<RwLock<HashMap<String, String>>>,
}

/// What a real node keeps on disk.
pub struct RealPaths {
    /// Private local preparation output.
    pub registry: std::path::PathBuf,
    /// Compiled templates, one `.meta` and `.cwasm` pair per
    /// `(module hash, engine version, micro-architecture)`. Apart from the
    /// registry because these files are mapped and never rewritten.
    pub artifacts: std::path::PathBuf,
}

impl RealPaths {
    pub fn under(root: &Path) -> Self {
        RealPaths {
            registry: root.join("registry"),
            artifacts: root.join("artifacts"),
        }
    }
}

impl RealCells {
    pub fn cells(&self) -> &[Arc<Cell>] {
        &self.cells
    }

    /// Local preparation using this node's existing compiler-owned artifact cache.
    pub fn local_preparation(&self) -> anyhow::Result<wisp_prepare::LocalPreparation> {
        // SAFETY: the node owns this cache and uses only the atomic, content-addressed
        // compiler writer, the same provenance required by its legacy mapped loader.
        let cache = unsafe {
            wisp_engine::CompilationCache::from_trusted_directory(self.artifacts.clone())
        }?;
        Ok(wisp_prepare::LocalPreparation::with_cache(Arc::new(cache))?)
    }

    /// Registers a prepared template without acquiring artifacts or invoking preparation.
    pub async fn install_prepared(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        manifest: wisp_abi::config::AppManifest,
        template: wisp_engine::Template,
    ) -> anyhow::Result<ModuleId> {
        let cell = self.cell_for(app)?;
        let module = ModuleId::new(app, version);
        let activator = EngineActivator::new(self.engine.clone(), template, module.clone());
        cell.register_module(tenant, Arc::new(activator), manifest)?;
        self.tenants
            .write()
            .insert(app.to_owned(), tenant.to_owned());
        self.modules.write().insert(module.clone());
        tracing::info!(app, version, cell = %cell.cfg.name, "prepared module registered");
        Ok(module)
    }

    /// The exact execution engine used to admit and activate this node's prepared templates.
    pub fn engine(&self) -> &wisp_engine::WispEngine {
        &self.engine
    }
}

/// Dispatches to the application's configured local cell.
#[async_trait::async_trait]
impl Dispatcher for RealCells {
    async fn dispatch_http(
        &self,
        target: &wisp_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        self.cell_for(&target.app)?.dispatch_http(target, req).await
    }

    async fn dispatch_http_stream(
        &self,
        target: &wisp_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<wisp_abi::host::HttpExchange> {
        self.cell_for(&target.app)?
            .dispatch_http_stream(target, req)
            .await
    }
    async fn dispatch_http_upload(
        &self,
        target: &wisp_abi::host::RouteTarget,
        upload: wisp_abi::host::HttpUpload,
    ) -> anyhow::Result<wisp_abi::host::HttpExchange> {
        self.cell_for(&target.app)?
            .dispatch_http_upload(target, upload)
            .await
    }
}

#[cfg(test)]
mod identity_tests;
