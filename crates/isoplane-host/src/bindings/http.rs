//! The `http_fetch`, `http_body_read` and `resp_*` import arms.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use isoplane_abi::calls::FetchRequest;
use isoplane_abi::dispatch::ResponseHead;
use isoplane_abi::result::{FutureError, ResultRecord};

use super::runtime::CellRuntime;
use super::{from_typed, CellBindings};

impl CellBindings {
    pub(super) fn req_body_read(&self, rt: Arc<CellRuntime>, id: u64, scalars: [u64; 3]) -> u64 {
        let Some(max_bytes) = (scalars[0] as i32)
            .try_into()
            .ok()
            .and_then(std::num::NonZeroU32::new)
        else {
            return self.settle_now(
                id,
                ResultRecord::Err(FutureError::Denied(
                    "request body read size must be positive".into(),
                )),
            );
        };
        let Some(scope) = self.ctx.request_body.lock().clone() else {
            return self.settle_now(
                id,
                ResultRecord::Err(FutureError::Denied(
                    "request body reads require an HTTP dispatch".into(),
                )),
            );
        };
        let read = match scope.begin_read() {
            Ok(read) => read,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let rt2 = rt.clone();
        let task = rt.reactor.spawn(async move {
            let result = match read.read(max_bytes).await {
                Ok(bytes) => ResultRecord::Ok(bytes),
                Err(error) => ResultRecord::Err(error),
            };
            CellBindings::settle(&rt2, id, result);
        });
        scope.attach_task(task.abort_handle());
        rt.futures.attach_task(id, task.abort_handle());
        id
    }

    pub(super) fn req_body_can_have_body(&self) -> i64 {
        let result = self
            .ctx
            .request_body
            .lock()
            .as_ref()
            .map(|scope| scope.can_have_body());
        match result {
            Some(Ok(present)) => i64::from(present),
            Some(Err(_)) | None => {
                self.ctx
                    .latch_trap(isoplane_abi::status::TrapCode::CapabilityViolation);
                0
            }
        }
    }

    /// Denies past the per-dispatch subrequest cap.
    fn check_subrequest(&self) -> Result<(), FutureError> {
        let n = self.ctx.subrequests.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.ctx.limits.subrequests_per_dispatch {
            return Err(FutureError::Denied(format!(
                "subrequest cap of {} per dispatch exceeded",
                self.ctx.limits.subrequests_per_dispatch
            )));
        }
        Ok(())
    }

    pub(super) fn http_fetch(&self, rt: Arc<CellRuntime>, id: u64, payload: &[u8]) -> u64 {
        let Some(req) = self.decode::<FetchRequest>(payload) else {
            return self.settle_now(
                id,
                ResultRecord::Err(FutureError::Denied("malformed fetch request".into())),
            );
        };
        if let Err(e) = self.check_subrequest() {
            rt.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
            return self.settle_now(id, ResultRecord::Err(e));
        }
        let owner =
            match crate::egress::ConnectionOwner::new(self.ctx.tenant.clone(), self.ctx.instance) {
                Ok(owner) => owner.with_exceptions(self.ctx.endpoint_exceptions.clone()),
                Err(error) => {
                    return self
                        .settle_now(id, ResultRecord::Err(FutureError::Io(error.to_string())))
                }
            };
        let scope = self.ctx.http.lock().clone();
        let mut operation = match scope.begin_fetch() {
            Ok(operation) => operation,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let handle = operation.handle();
        let allowlist = self.ctx.allowlist.clone();
        let exceptions = self.ctx.endpoint_exceptions.clone();
        let tenant = self.ctx.tenant.clone();
        let rt2 = rt.clone();
        let task = rt.reactor.spawn(async move {
            let svc = rt2.services.clone();
            let checked = svc
                .egress
                .check_owned(
                    &req.url,
                    crate::egress::Reach {
                        allowlist: allowlist.as_deref(),
                        exceptions: &exceptions,
                    },
                )
                .await;
            let res = match checked {
                Ok(pinned) => {
                    let target = pinned.target();
                    let out = match operation.retain_permit(pinned) {
                        Ok(()) => svc
                            .fetcher
                            .fetch(owner, target, req)
                            .await
                            .and_then(|response| operation.response(response)),
                        Err(error) => Err(error),
                    };
                    if let Ok(r) = &out {
                        svc.egress
                            .record_bytes(r.body_inline.as_ref().map_or(0, |b| b.len()) as u64);
                    }
                    from_typed(out)
                }
                Err(crate::egress::EgressError::Denied(deny)) => {
                    tracing::warn!(tenant = %tenant, reason = %deny, "egress denied");
                    rt2.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                    ResultRecord::Err(FutureError::Denied(deny.to_string()))
                }
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
        let scope = self.ctx.http.lock().clone();
        let (operation, body) = match scope.begin_read(handle) {
            Ok(read) => read,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let resource = operation.handle();
        let rt2 = rt.clone();
        let task = rt.reactor.spawn(async move {
            let result = operation.read(body, max).await;
            if let Ok(chunk) = &result {
                rt2.services.egress.record_bytes(chunk.data.len() as u64);
            }
            CellBindings::settle(&rt2, id, from_typed(result));
        });
        scope.attach_task(resource, task.abort_handle());
        rt.futures.attach_task(id, task.abort_handle());
        id
    }

    pub(super) fn resp_write(
        &self,
        rt: Arc<CellRuntime>,
        id: u64,
        payload: &[u8],
        scalars: [u64; 3],
    ) -> u64 {
        let handle = scalars[0] as u32;
        let stream = {
            let responses = self.ctx.responses.lock();
            responses
                .get(handle)
                .filter(|response| response.head.streaming && !response.finished)
                .and(responses.transport.clone())
        };
        if let Some(stream) = stream {
            let write = stream.prepare(payload.to_vec());
            let rt2 = rt.clone();
            let task = rt.reactor.spawn(async move {
                let result = write.send().await.map(|count| count as u32);
                CellBindings::settle(&rt2, id, from_typed(result));
            });
            rt.futures.attach_task(id, task.abort_handle());
            return id;
        }
        let res = match self.ctx.responses.lock().write(handle, payload.to_vec()) {
            Ok(n) => ResultRecord::Ok(postcard::to_allocvec(&(n as u32)).unwrap_or_default()),
            Err(e) => ResultRecord::Err(FutureError::Io(format!("resp_write: {e:?}"))),
        };
        self.settle_now(id, res)
    }

    pub(super) fn resp_new(&self, payload: &[u8]) -> i64 {
        let Some(mut head) = self.decode::<ResponseHead>(payload) else {
            return 0;
        };
        let transport = self.ctx.responses.lock().transport.clone();
        if head.streaming {
            if let Some(stream) = transport {
                if let Err(error) = stream.start(&head) {
                    tracing::warn!(instance = %self.ctx.instance, ?error, "response start refused");
                    self.ctx
                        .latch_trap(isoplane_abi::status::TrapCode::Protocol);
                    return 0;
                }
                if !head.body.is_empty() {
                    let write = stream.prepare(std::mem::take(&mut head.body));
                    if let Some(rt) = self.rt() {
                        let owner = stream.clone();
                        let task = rt.reactor.spawn(async move {
                            if let Err(error) = write.send().await {
                                stream.fail(format!("initial response body: {error:?}"));
                            }
                        });
                        owner.own_initial_write(task.abort_handle());
                    }
                }
            }
        }
        self.ctx.responses.lock().create(head) as i64
    }

    pub(super) fn resp_finish(&self, scalars: [u64; 3]) -> i64 {
        let h = scalars[0] as u32;
        match self.ctx.responses.lock().finish(h) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{call, ctx};
    use super::*;
    use isoplane_engine::HostBindings;

    #[test]
    fn responses_are_built_through_the_vtable() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let head = ResponseHead {
            status: 201,
            headers: vec![],
            body: b"hi".to_vec(),
            streaming: false,
        };
        let h = b.call_sync(call(
            "resp_new",
            &postcard::to_allocvec(&head).expect("encode"),
            [0; 3],
        ));
        assert_eq!(h, 1, "the first response handle");
        assert_eq!(
            b.call_sync(call("resp_finish", &[], [h as u64, 0, 0])),
            0,
            "finishing a live handle succeeds"
        );
        assert_eq!(
            b.call_sync(call("resp_finish", &[], [99, 0, 0])),
            -1,
            "finishing an unknown handle is refused"
        );
        let r = c.responses.lock().take(h as u32).expect("the response");
        assert_eq!(r.head.status, 201, "the head is the guest's");
        assert!(r.finished, "and it is finished");
    }
}
