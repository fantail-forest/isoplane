//! Adapts HTTP requests to cell dispatches.

use async_trait::async_trait;
use isoplane_abi::dispatch::HttpRequest;
use isoplane_abi::host::{Dispatcher, HttpExchange, HttpUpload, RouteTarget};
use tracing::instrument::WithSubscriber;
use tracing::Instrument;

use crate::cell::{Cell, Target};
use crate::ModuleId;

impl Cell {
    async fn run_http_exchange(
        &self,
        target: &RouteTarget,
        req: HttpRequest,
        upload: std::sync::Arc<crate::bindings::request_body::RequestBodyScope>,
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
        let exchange_task = async move {
            let stream = output.clone();
            let result = cell
                .dispatch_http_async(
                    &module,
                    Target::Handler { route },
                    record,
                    Some(crate::cell::HttpDispatch {
                        response: stream,
                        upload: Some(upload),
                    }),
                )
                .await;
            match result {
                Ok(result) => {
                    if output.has_started() {
                        if let Some(error) = result.trap {
                            output.fail(error);
                        } else {
                            output.finish();
                        }
                    } else {
                        let mut head = isoplane_abi::ResponseHead {
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
                Err(error) => output.fail(format!("dispatch failed: {error}")),
            }
            if !output.has_owner() {
                output.acknowledge_settlement();
            }
            output.record_buffers(dispatch_id);
        };
        self.reactor().spawn(
            exchange_task
                .instrument(tracing::Span::current())
                .with_current_subscriber(),
        );
        transport.exchange().await
    }

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
}

#[async_trait]
impl Dispatcher for Cell {
    async fn dispatch_http_upload(
        &self,
        target: &RouteTarget,
        upload: HttpUpload,
    ) -> anyhow::Result<HttpExchange> {
        let body = if !upload.can_have_body || upload.body.remaining_bytes() == Some(0) {
            None
        } else {
            Some(upload.body)
        };
        let source =
            crate::bindings::request_body::RequestBodyScope::new(body, upload.can_have_body);
        self.run_http_exchange(target, upload.request, source).await
    }
}
