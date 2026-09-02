//! Deliver HTTP requests from the proxy to the cell execution pool.

use std::time::Duration;

use async_trait::async_trait;
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::host::{Dispatcher, HttpReply, RouteTarget};

use crate::cell::{Cell, DispatchResult, Target};
use crate::ModuleId;

/// The wall cap plus a margin: the budget and the wall cap fire inside the
/// cell, and this only keeps a caller from waiting forever on a cell bug.
fn deadline(cell: &Cell) -> Duration {
    Duration::from_millis(cell.cfg.wall_ms_cap as u64 + 5_000)
}

impl Cell {
    fn module_for(&self, target: &RouteTarget) -> anyhow::Result<ModuleId> {
        let module = ModuleId::new(&target.app, &target.version);
        let tenant = self.module_tenant(&module).ok_or_else(|| {
            anyhow::anyhow!(
                "cell `{}` has no template for {module}; \
                 the version was never registered here",
                self.cfg.name
            )
        })?;
        // The other half of `Cell::admits`: a route to the right module and
        // the wrong tenant is not served.
        anyhow::ensure!(
            tenant == target.tenant,
            "{module} on cell `{}` belongs to tenant `{tenant}`, not `{}`",
            self.cfg.name,
            target.tenant
        );
        Ok(module)
    }

    /// A `Dispatcher` method runs on a reactor thread, so the blocking
    /// dispatch is handed to a blocking task.
    async fn run(
        &self,
        module: ModuleId,
        target: Target,
        rec: DispatchRecord,
    ) -> anyhow::Result<DispatchResult> {
        let me = self
            .me()
            .ok_or_else(|| anyhow::anyhow!("cell `{}` is shutting down", self.cfg.name))?;
        let timeout = deadline(self);
        tokio::task::spawn_blocking(move || me.dispatch_blocking(&module, target, rec, timeout))
            .await?
    }


}



#[async_trait]
impl Dispatcher for Cell {
    async fn dispatch_http(
        &self,
        target: &RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        let module = self.module_for(target)?;
        let route = req.path.clone();
        let rec = self.http_record(&module, req);
        let out = self.run(module, Target::Handler { route }, rec).await?;

        if let Some(trap) = &out.trap {
            tracing::warn!(
                app = %target.app, version = %target.version, trap,
                "guest fault"
            );
        }
        Ok(HttpReply {
            status: out.status,
            headers: out.headers,
            body: out.body,
            // Chunks written after `done` are not piped to the requester.
            streaming: false,
        })
    }





}
