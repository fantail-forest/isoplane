//! The `req_body_*` and `resp_*` import arms.

use std::sync::Arc;

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
        let Some(scope) = self.ctx.current().request_body.lock().clone() else {
            return self.settle_now(
                id,
                ResultRecord::Err(FutureError::Denied(
                    "request body reads require an HTTP dispatch".into(),
                )),
            );
        };
        let mut read = match scope.begin_read() {
            Ok(read) => read,
            Err(error) => return self.settle_now(id, ResultRecord::Err(error)),
        };
        let rt2 = rt.clone();
        let task = self.ctx.spawn(&rt.reactor, async move {
            // A slow upload retains its transport frame, not a reservation
            // that would prevent another request's available data progressing.
            let available = match read.prepare(max_bytes.get() as usize).await {
                Ok(0) => {
                    CellBindings::settle(&rt2, id, ResultRecord::Ok(vec![]));
                    return;
                }
                Ok(available) => available,
                Err(error) => {
                    CellBindings::settle(&rt2, id, ResultRecord::Err(error));
                    return;
                }
            };
            let space = match rt2.futures.reserve_read(id.into(), available).await {
                Ok(space) => space,
                Err(error) => {
                    CellBindings::settle(&rt2, id, ResultRecord::Err(error));
                    return;
                }
            };
            let result = match read.consume(space.bytes()) {
                Ok(bytes) => ResultRecord::Ok(bytes),
                Err(error) => ResultRecord::Err(error),
            };
            let target = rt2
                .futures
                .complete_read(result, space, Some(Box::new(read)));
            if let crate::futures::WakeTarget::Instance(owner) = target {
                rt2.wake.signal(owner);
            }
        });
        scope.attach_task(task.abort_handle());
        rt.futures.attach_task(id, task.abort_handle());
        id
    }

    pub(super) fn req_body_can_have_body(&self) -> i64 {
        let result = self
            .ctx
            .current()
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
                .and_then(|response| self.ctx.dispatch(response.dispatch))
                .and_then(|dispatch| dispatch.transport.lock().clone())
        };
        if let Some(stream) = stream {
            let write = stream.prepare(payload.to_vec());
            let rt2 = rt.clone();
            let task = self.ctx.spawn(&rt.reactor, async move {
                let result = write.send().await.map(|count| count as u32);
                CellBindings::settle(&rt2, id, from_typed(result));
            });
            rt.futures.attach_task(id, task.abort_handle());
            return id;
        }
        let res = match self.ctx.responses.lock().write(handle, payload) {
            Ok(n) => super::ok_bytes(&(n as u32)),
            Err(crate::response::ResponseError::BufferLimit) => {
                ResultRecord::Err(FutureError::BufferLimit)
            }
            Err(error) => ResultRecord::Err(FutureError::Io(format!("resp_write: {error}"))),
        };
        self.settle_now(id, res)
    }

    pub(super) fn resp_new(&self, payload: &[u8]) -> i64 {
        let dispatch = self.ctx.current();
        let mut responses = self.ctx.responses.lock();
        let head = match responses.decode_head(payload) {
            Ok(head) => head,
            Err(error) => return self.response_refused(error),
        };
        let handle = match responses.create(dispatch.id(), head) {
            Ok(handle) => handle,
            Err(error) => return self.response_refused(error),
        };
        let transport = dispatch.transport.lock().clone();
        if let Some(stream) = transport {
            let response = responses
                .get(handle)
                .expect("the response was inserted above while holding the table lock");
            if response.head.streaming {
                match stream.start(&response.head) {
                    Ok(()) => {
                        let body = responses
                            .take_body(handle)
                            .expect("the response remains inserted while holding the table lock");
                        if !body.is_empty() {
                            let write = stream.prepare(body);
                            if let Some(rt) = self.rt() {
                                let owner = stream.clone();
                                let task = self.ctx.spawn(&rt.reactor, async move {
                                    if let Err(error) = write.send().await {
                                        stream.fail(format!("initial response body: {error:?}"));
                                    }
                                });
                                owner.own_initial_write(task.abort_handle());
                            }
                        }
                    }
                    // The requester left first, which a guest cannot know
                    // when it answers: the response goes nowhere and its
                    // writes are answered as cancelled.
                    Err(FutureError::Cancelled) => {}
                    Err(error) => {
                        responses.take(handle);
                        tracing::warn!(instance = %self.ctx.instance, ?error, "response start refused");
                        self.ctx
                            .latch_trap(isoplane_abi::status::TrapCode::Protocol);
                        return 0;
                    }
                }
            }
        }
        i64::from(handle)
    }

    fn response_refused(&self, error: crate::response::ResponseError) -> i64 {
        use crate::response::ResponseError;
        let code = match error {
            ResponseError::Malformed(_)
            | ResponseError::NoSuchHandle
            | ResponseError::AlreadyFinished => isoplane_abi::status::TrapCode::Protocol,
            ResponseError::BufferLimit | ResponseError::HandleLimit => {
                isoplane_abi::status::TrapCode::OutOfMemory
            }
        };
        tracing::warn!(tenant = %self.ctx.tenant, instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(), %error, "response creation refused");
        self.ctx.latch_trap(code);
        0
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
    use isoplane_abi::dispatch::ResponseHead;
    use isoplane_engine::HostBindings;

    #[test]
    fn a_response_budget_refusal_cannot_publish_streaming_headers() {
        let mut context = ctx();
        Arc::get_mut(&mut context)
            .expect("new context")
            .configure_resources(super::super::GuestResourceLimits {
                response_handles: 0,
                ..Default::default()
            });
        context.begin_dispatch(1, 1);
        let transport = crate::response_transport::ResponseTransport::new(1024);
        *context.current().transport.lock() = Some(transport.clone());
        let mut bindings = CellBindings::new(context.clone());
        let head = ResponseHead {
            status: 200,
            headers: vec![],
            body: b"initial".to_vec(),
            streaming: true,
        };
        assert_eq!(
            bindings.call_sync(call(
                "resp_new",
                &postcard::to_allocvec(&head).expect("encode"),
                [0; 3]
            )),
            0
        );
        assert!(
            !transport.has_started(),
            "table admission happens before transport side effects"
        );
        assert_eq!(
            context.take_trap(),
            Some(isoplane_abi::status::TrapCode::OutOfMemory)
        );
    }

    #[test]
    fn a_streaming_response_begun_after_the_requester_left_is_not_a_guest_fault() {
        use futures::FutureExt;
        let context = ctx();
        context.begin_dispatch(1, 1);
        let transport = crate::response_transport::ResponseTransport::new(1024);
        *context.current().transport.lock() = Some(transport.clone());
        assert!(
            transport.exchange().now_or_never().is_none(),
            "no response headers exist for the requester to receive"
        );
        assert!(
            transport.is_cancelled(),
            "the requester left before headers"
        );
        let mut bindings = CellBindings::new(context.clone());
        let head = ResponseHead {
            status: 499,
            headers: vec![],
            body: b"cancelled".to_vec(),
            streaming: true,
        };
        let handle = bindings.call_sync(call(
            "resp_new",
            &postcard::to_allocvec(&head).expect("encode"),
            [0; 3],
        ));
        assert_ne!(handle, 0, "the guest gets a response to finish");
        assert_eq!(
            context.take_trap(),
            None,
            "answering a requester that has left is not a protocol violation"
        );
        assert!(
            !transport.has_started(),
            "nothing is sent to the departed requester"
        );
        assert_eq!(
            bindings.call_sync(call("resp_finish", &[], [handle as u64, 0, 0])),
            0,
            "and the guest can finish it"
        );
    }

    #[test]
    fn a_second_streaming_response_for_one_dispatch_is_a_protocol_trap() {
        let context = ctx();
        context.begin_dispatch(1, 1);
        let transport = crate::response_transport::ResponseTransport::new(1024);
        *context.current().transport.lock() = Some(transport.clone());
        let mut bindings = CellBindings::new(context.clone());
        let head = postcard::to_allocvec(&ResponseHead {
            status: 200,
            headers: vec![],
            body: vec![],
            streaming: true,
        })
        .expect("encode");
        assert_ne!(bindings.call_sync(call("resp_new", &head, [0; 3])), 0);
        assert_eq!(
            bindings.call_sync(call("resp_new", &head, [0; 3])),
            0,
            "a dispatch has one response to start"
        );
        assert_eq!(
            context.take_trap(),
            Some(isoplane_abi::status::TrapCode::Protocol)
        );
    }

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
