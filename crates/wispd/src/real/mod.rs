//! Local preparation and execution cells behind the HTTP dispatcher.

mod admin;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{Dispatcher, HttpReply, RouteTarget};
use wisp_host::ModuleId;
use wisp_host::{Cell, EngineActivator};

pub use admin::RealNodeAdmin;

/// The node's cells on one engine, and the placement that puts an app on
/// one of them.
pub struct RealCells {
    engine: Arc<wisp_engine::WispEngine>,
    cells: Vec<Arc<Cell>>,
    /// Configured application-to-cell assignment.
    placement: RwLock<HashMap<String, usize>>,
    /// `app@version` -> the module registered for it.
    modules: RwLock<HashMap<String, ModuleId>>,
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

    fn key(app: &str, version: &str) -> String {
        format!("{app}@{version}")
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
}
