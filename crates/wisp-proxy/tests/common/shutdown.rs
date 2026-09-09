use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use wisp_abi::dispatch::{HttpRequest, };
use wisp_abi::host::{
Dispatcher, HttpBody, HttpExchange, HttpReply, HttpResponseBody, ResponseDisposition,
    RouteTarget,
};
use crate::common::fake::FakeCell;

pub struct BlockedResponse {
    pub cell: Arc<FakeCell>,
    pub polled: CancellationToken,
    pub dropped: CancellationToken,
}

struct PendingBody {
    polled: CancellationToken,
    dropped: CancellationToken,
}

impl Drop for PendingBody {
    fn drop(&mut self) {
        self.dropped.cancel();
    }
}

#[async_trait]
impl HttpBody for PendingBody {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        self.polled.cancel();
        std::future::pending().await
    }
}

#[async_trait]
impl HttpResponseBody for PendingBody {
    async fn finish(self: Box<Self>, _: ResponseDisposition) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl Dispatcher for BlockedResponse {
    async fn dispatch_http(
        &self,
        target: &RouteTarget,
        request: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        self.cell.dispatch_http(target, request).await
    }
    async fn dispatch_http_stream(
        &self,
        _: &RouteTarget,
        _: HttpRequest,
    ) -> anyhow::Result<HttpExchange> {
        Ok(HttpExchange {
            status: 200,
            headers: vec![],
            body: Box::new(PendingBody {
                polled: self.polled.clone(),
                dropped: self.dropped.clone(),
            }),
        })
    }
}
