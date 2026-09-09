//! Deliver HTTP requests from the proxy to the cell execution pool.

use std::time::Duration;

use async_trait::async_trait;
use wisp_abi::dispatch::{DispatchRecord, HttpRequest};
use wisp_abi::host::{Dispatcher, HttpExchange, HttpReply, RouteTarget};

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

    async fn dispatch_http_stream(
        &self,
        target: &RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpExchange> {
        let module = self.module_for(target)?;
        let route = req.path.clone();
        let record = self.http_record(&module, req);
        let dispatch_id = record.dispatch_id;
        let cell = self
            .me()
            .ok_or_else(|| anyhow::anyhow!("cell is shutting down"))?;
        let transport =
            crate::response_transport::ResponseTransport::new(self.cfg.response_queue_bytes);
        let output = transport.clone();
        let timeout = deadline(self);
        self.reactor().spawn(async move {
            let stream = output.clone();
            let result = tokio::task::spawn_blocking(move || {
                cell.dispatch_with_stream(
                    &module,
                    Target::Handler { route },
                    record,
                    timeout,
                    Some(stream),
                )
            })
            .await;
            match result {
                Ok(Ok(result)) => {
                    if output.has_started() {
                        if let Some(error) = result.trap {
                            output.fail(error);
                        } else {
                            output.finish();
                        }
                    } else {
                        let mut head = wisp_abi::ResponseHead {
                            status: result.status,
                            headers: result.headers,
                            body: result.body,
                            streaming: false,
                        };
                        match output.start(&head) {
                            Ok(()) => {
                                match output.prepare(std::mem::take(&mut head.body)).send().await {
                                    Ok(_) => output.finish(),
                                    Err(error) => {
                                        output.fail(format!("buffered response: {error:?}"))
                                    }
                                }
                            }
                            Err(error) => output.fail(format!("response start: {error:?}")),
                        }
                    }
                }
                Ok(Err(error)) => output.fail(format!("dispatch failed: {error}")),
                Err(error) => output.fail(format!("dispatch task failed: {error}")),
            }
            output.record_buffers(dispatch_id);
        });
        transport.exchange().await
    }





}
