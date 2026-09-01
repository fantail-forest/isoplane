//! Deliver HTTP requests from the proxy to the cell execution pool.

use std::time::Duration;

use async_trait::async_trait;
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::host::{Dispatcher, HttpReply, RouteTarget};

use crate::cell::{Cell, DispatchResult, Target};
use crate::ModuleId;

/// A dispatch that is still outstanding after this long has not parked — it
/// has gone wrong. The real backstop is the CPU budget and
/// `CellConfig::wall_ms_cap`, both of which fire inside the cell; this only
/// exists so a caller cannot be blocked forever by a bug in the cell itself,
/// which is why it is the wall cap plus a margin rather than a limit in its
/// own right.
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
        // Trust-tier partitioning is enforced at registration by
        // `Cell::admits`. This is the other half: a route that resolved to the
        // right module but the wrong tenant must not be served, or the tier
        // partitioning would be a property of the route table rather than of
        // the cell.
        anyhow::ensure!(
            tenant == target.tenant,
            "{module} on cell `{}` belongs to tenant `{tenant}`, not `{}`",
            self.cfg.name,
            target.tenant
        );
        Ok(module)
    }

    /// Run a dispatch on the exec pool without blocking the caller's reactor.
    ///
    /// Reactor threads own I/O, execution-pool threads run guests. A
    /// `Dispatcher` method is called *on* a reactor thread, so the handoff has
    /// to happen here — `Cell::dispatch_blocking` would otherwise park a proxy
    /// worker for the length of a guest dispatch.
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
            // The trap rate has to be visible, and the trap has already
            // become a 5xx inside the cell, so this is telemetry, not control
            // flow.
            tracing::warn!(
                app = %target.app, version = %target.version, trap,
                "guest fault"
            );
        }
        Ok(HttpReply {
            status: out.status,
            headers: out.headers,
            body: out.body,
            // Streaming responses accumulate on the instance and are
            // readable via `Cell::response_snapshot`, but the PoC does not
            // pipe post-`done` chunks to the requester. Claiming `streaming`
            // here would promise the proxy chunks that never arrive.
            streaming: false,
        })
    }





}
