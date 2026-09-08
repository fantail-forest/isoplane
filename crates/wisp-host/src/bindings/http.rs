//! The `http_fetch`, `http_body_read` and `resp_*` import arms.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use wisp_abi::calls::FetchRequest;
use wisp_abi::dispatch::ResponseHead;
use wisp_abi::result::{FutureError, ResultRecord};

use super::runtime::CellRuntime;
use super::{from_typed, spawn, CellBindings};

impl CellBindings {
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
        // The check blocks on `getaddrinfo`, so it runs on the reactor's
        // blocking pool.
        let allowlist = self.ctx.allowlist.clone();
        let tenant = self.ctx.tenant.clone();
        let rt2 = rt.clone();
        let task = rt.reactor.spawn(async move {
            let svc = rt2.services.clone();
            let checked = {
                let egress = svc.egress.clone();
                let url = req.url.clone();
                tokio::task::spawn_blocking(move || egress.check_owned(&url, allowlist.as_deref()))
                    .await
            };
            let res = match checked {
                Ok(Ok(pinned)) => {
                    let out = svc.fetcher.fetch(pinned.target(), req).await;
                    if let Ok(r) = &out {
                        svc.egress
                            .record_bytes(r.body_inline.as_ref().map_or(0, |b| b.len()) as u64);
                    }
                    from_typed(out)
                }
                Ok(Err(deny)) => {
                    tracing::warn!(tenant = %tenant, reason = %deny, "egress denied");
                    rt2.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                    ResultRecord::Err(FutureError::Denied(deny.to_string()))
                }
                Err(e) => ResultRecord::Err(FutureError::Io(format!("egress check: {e}"))),
            };
            CellBindings::settle(&rt2, id, res);
        });
        rt.futures.attach_task(id, task.abort_handle());
        id
    }

    pub(super) fn http_body_read(&self, rt: Arc<CellRuntime>, id: u64, scalars: [u64; 3]) -> u64 {
        let handle = scalars[0] as u32;
        let max = scalars[1] as usize;
        spawn!(rt, id, |svc| from_typed(
            svc.fetcher.read_body(handle, max).await
        ))
    }

    pub(super) fn resp_write(&self, id: u64, payload: &[u8], scalars: [u64; 3]) -> u64 {
        let handle = scalars[0] as u32;
        let res = match self.ctx.responses.lock().write(handle, payload.to_vec()) {
            Ok(n) => ResultRecord::Ok(postcard::to_allocvec(&(n as u32)).unwrap_or_default()),
            Err(e) => ResultRecord::Err(FutureError::Io(format!("resp_write: {e:?}"))),
        };
        self.settle_now(id, res)
    }

    pub(super) fn resp_new(&self, payload: &[u8]) -> i64 {
        let Some(head) = self.decode::<ResponseHead>(payload) else {
            return 0;
        };
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
    use wisp_engine::HostBindings;

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
