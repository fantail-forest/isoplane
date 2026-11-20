//! The `http_fetch` and `http_body_read` import arms.

use std::sync::Arc;

use isoplane_abi::calls::FetchRequest;
use isoplane_abi::result::{FutureError, ResultRecord};

use super::runtime::CellRuntime;
use super::{from_typed, CellBindings};

impl CellBindings {
    pub(super) fn http_fetch(&self, rt: Arc<CellRuntime>, id: u64, payload: &[u8]) -> u64 {
        let Some(req) = self.decode::<FetchRequest>(payload) else {
            return self.settle_now(
                id,
                ResultRecord::Err(FutureError::Denied("malformed fetch request".into())),
            );
        };
        let owner =
            match crate::egress::ConnectionOwner::new(self.ctx.tenant.clone(), self.ctx.instance) {
                Ok(owner) => owner
                    .with_application_limits(&self.ctx.limits)
                    .with_exceptions(self.ctx.endpoint_exceptions.clone()),
                Err(error) => {
                    return self
                        .settle_now(id, ResultRecord::Err(FutureError::Io(error.to_string())))
                }
            };
        let scope = self.ctx.http.clone();
        let operation = match scope.begin_fetch() {
            Ok(operation) => operation,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let handle = operation.handle();
        let allowlist = self.ctx.allowlist.clone();
        let exceptions = self.ctx.endpoint_exceptions.clone();
        let tenant = self.ctx.tenant.clone();
        let rt2 = rt.clone();
        let task = self.ctx.spawn(&rt.reactor, async move {
            let svc = rt2.services.clone();
            let denied = |deny: crate::egress::DenyReason| {
                tracing::warn!(tenant = %tenant, reason = %deny, "egress denied");

                ResultRecord::Err(FutureError::Denied(deny.to_string()))
            };
            let checked = svc
                .egress
                .check_url(
                    &req.url,
                    crate::egress::Reach {
                        allowlist: allowlist.as_deref(),
                        exceptions: &exceptions,
                    },
                )
                .await;
            let res = match checked {
                Ok(target) => match svc.egress.begin_connect(owner).await {
                    Ok(attempt) => from_typed(
                        svc.fetcher
                            .fetch(attempt, target, req)
                            .await
                            .and_then(|response| operation.response(response)),
                    ),
                    Err(crate::egress::ConnectionError::Denied(deny)) => denied(deny),
                    Err(
                        error @ (crate::egress::ConnectionError::Io(_)
                        | crate::egress::ConnectionError::PeerMismatch { .. }),
                    ) => ResultRecord::Err(FutureError::Io(error.to_string())),
                },
                Err(crate::egress::EgressError::Denied(deny)) => denied(deny),
                Err(crate::egress::EgressError::Dns(error)) => {
                    tracing::warn!(tenant = %tenant, %error, "outbound HTTP DNS failed");
                    ResultRecord::Err(FutureError::Io(error.to_string()))
                }
            };
            CellBindings::settle(&rt2, id, res);
        });
        scope.attach_task(handle, task.abort_handle());
        rt.futures.attach_task(id, task.abort_handle());
        id
    }

    pub(super) fn http_body_read(&self, rt: Arc<CellRuntime>, id: u64, scalars: [u64; 3]) -> u64 {
        let handle = scalars[0] as u32;
        let max = scalars[1] as usize;
        let scope = self.ctx.http.clone();
        let (operation, body) = match scope.begin_read(handle) {
            Ok(read) => read,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let resource = operation.handle();
        let rt2 = rt.clone();
        let task = self.ctx.spawn(&rt.reactor, async move {
            // Postcard encodes the byte length as a varint and EOF as one byte.
            const OVERHEAD_BYTES: usize = (usize::BITS as usize).div_ceil(7) + 1;
            let requested = if max == 0 {
                usize::MAX
            } else {
                max.saturating_add(OVERHEAD_BYTES)
            };
            let space = match rt2.futures.reserve_read(id.into(), requested).await {
                Ok(space) => space,
                Err(error) => {
                    CellBindings::settle(&rt2, id, ResultRecord::Err(error));
                    return;
                }
            };
            let max = space.bytes().saturating_sub(OVERHEAD_BYTES);
            let result = if max == 0 {
                ResultRecord::Err(FutureError::BufferLimit)
            } else {
                match from_typed(operation.read(body, max).await) {
                    ResultRecord::Ok(bytes) => {
                        ResultRecord::Ok(bytes.into_boxed_slice().into_vec())
                    }
                    error @ ResultRecord::Err(_) => error,
                }
            };
            if let crate::futures::WakeTarget::Instance(owner) =
                rt2.futures.complete_read(result, space, None)
            {
                rt2.wake.signal(owner);
            }
        });
        scope.attach_task(resource, task.abort_handle());
        rt.futures.attach_task(id, task.abort_handle());
        id
    }
}
