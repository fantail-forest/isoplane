//! Instance-owned deadlines retained independently of the Wasmtime Store.

use super::CellBindings;
use crate::futures::{FutureId, FutureTable};
use crate::InstanceId;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wisp_abi::native_io::{Error, Request, ResourceHandle, Response};
use wisp_abi::result::ResultRecord;

struct ReactorWork {
    future: FutureId,
    owner: InstanceId,
    table: Arc<FutureTable>,
    task: tokio::task::AbortHandle,
}

impl Drop for ReactorWork {
    fn drop(&mut self) {
        self.task.abort();
        self.table.cancel_owned(self.owner, self.future);
    }
}

/// Maximum simultaneously retained native handles for an instance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceCount(pub usize);

/// Bounds the host records retained on behalf of one instance.
pub(crate) struct ResourceLimits {
    pub handles: ResourceCount,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            handles: ResourceCount(1024),
        }
    }
}

struct Timer {
    ready: Arc<AtomicBool>,
    _task: Option<ReactorWork>,
}

pub(crate) struct Resources {
    next: u32,
    limits: ResourceLimits,
    entries: BTreeMap<ResourceHandle, Timer>,
}

impl Resources {
    pub(crate) fn new(limits: ResourceLimits) -> Self {
        Self {
            next: 1,
            limits,
            entries: BTreeMap::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    fn reserve(&self, count: ResourceCount) -> Result<(), Error> {
        if self.entries.len().saturating_add(count.0) > self.limits.handles.0
            || self
                .next
                .checked_add(u32::try_from(count.0).map_err(|_| Error::ResourceLimit)?)
                .is_none()
        {
            return Err(Error::ResourceLimit);
        }
        Ok(())
    }

    fn insert(&mut self, timer: Timer) -> Result<ResourceHandle, Error> {
        self.reserve(ResourceCount(1))?;
        let handle = ResourceHandle(self.next);
        self.next += 1;
        self.entries.insert(handle, timer);
        Ok(handle)
    }

    fn ready(&self, handle: ResourceHandle) -> Result<bool, Error> {
        self.entries
            .get(&handle)
            .map(|timer| timer.ready.load(Ordering::Acquire))
            .ok_or(Error::InvalidHandle(handle))
    }

    pub(crate) fn call(
        &mut self,
        request: Request,
        bindings: &CellBindings,
    ) -> Result<Response, Error> {
        match request {
            Request::Timer(duration) => {
                self.reserve(ResourceCount(1))?;
                let ready = Arc::new(AtomicBool::new(duration.0 == 0));
                let task = if duration.0 == 0 {
                    None
                } else {
                    let runtime = bindings
                        .rt()
                        .ok_or(Error::Unsupported("native timer without reactor"))?
                        .clone();
                    let future = bindings
                        .alloc(
                            wisp_abi::native_io::canonical::POLL_IMPORT,
                        )
                        .ok_or(Error::ResourceLimit)?;
                    let completion = runtime.clone();
                    let ready = ready.clone();
                    let task = runtime.reactor.spawn(async move {
                        let duration_ms =
                            duration.0.div_ceil(wisp_abi::pal::CLOCK_QUANTUM_NS as u64);
                        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                        ready.store(true, Ordering::Release);
                        completion.settle_future(future, ResultRecord::Ok(Vec::new()));
                    });
                    runtime.futures.attach_task(future, task.abort_handle());
                    Some(ReactorWork {
                        future: FutureId::from(future),
                        owner: bindings.ctx.instance,
                        table: runtime.futures.clone(),
                        task: task.abort_handle(),
                    })
                };
                self.insert(Timer { ready, _task: task })
                    .map(Response::Handle)
            }

            Request::Ready(handle) => self.ready(handle).map(Response::Ready),
            Request::Poll(handles) => {
                let mut indexes = Vec::new();
                for (index, handle) in handles.into_iter().enumerate() {
                    if self.ready(handle)? {
                        indexes.push(u32::try_from(index).map_err(|_| Error::ResourceLimit)?);
                    }
                }
                if indexes.is_empty() {
                    return Err(Error::Unsupported("blocking poll on a guest stack"));
                }
                Ok(Response::ReadyIndexes(indexes))
            }
            Request::Drop(handle) => {
                self.entries
                    .remove(&handle)
                    .ok_or(Error::InvalidHandle(handle))?;
                Ok(Response::Done)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::native_io::DurationNanos;

    fn resources(handles: usize) -> (Resources, CellBindings) {
        (
            Resources::new(ResourceLimits {
                handles: ResourceCount(handles),
            }),
            CellBindings::new(super::super::tests::ctx()),
        )
    }

    fn created(resources: &mut Resources, bindings: &CellBindings) -> ResourceHandle {
        match resources
            .call(Request::Timer(DurationNanos(0)), bindings)
            .expect("an available resource slot must accept a ready pollable")
        {
            Response::Handle(handle) => handle,
            response => panic!("pollable allocation returned an unexpected result: {response:?}"),
        }
    }

    #[test]
    fn cancellation_refuses_new_deadlines_but_allows_polling_and_disposal() {
        use wisp_engine::HostBindings;
        let context = super::super::tests::ctx();
        let mut bindings = CellBindings::new(context.clone());
        context.poison_imports();
        assert!(
            matches!(
                bindings.native_io(Request::Timer(DurationNanos(1))),
                Err(Error::Denied(_))
            ),
            "a cancelled dispatch must not start reactor work"
        );
        let Response::Handle(timer) = bindings
            .native_io(Request::Timer(DurationNanos(0)))
            .expect("cancellation cleanup must retain nonblocking polling")
        else {
            panic!("a ready pollable must return a handle")
        };
        assert!(
            matches!(bindings.native_io(Request::Poll(vec![timer])), Ok(Response::ReadyIndexes(indexes)) if indexes == vec![0]),
            "cancellation cleanup must be able to inspect its ready pollable"
        );
        bindings
            .native_io(Request::Drop(timer))
            .expect("a cancelled dispatch must be allowed to dispose native resources");
    }

    #[test]
    fn exhausting_native_handles_refuses_allocation_until_a_resource_is_dropped() {
        let (mut resources, bindings) = resources(1);
        let handle = created(&mut resources, &bindings);
        assert!(
            matches!(
                resources.call(Request::Timer(DurationNanos(0)), &bindings),
                Err(Error::ResourceLimit)
            ),
            "a second pollable must exceed the instance's one-handle budget"
        );
        resources
            .call(Request::Drop(handle), &bindings)
            .expect("the owner must release its pollable");
        let replacement = created(&mut resources, &bindings);
        assert_ne!(
            handle, replacement,
            "a dropped handle must never alias a replacement pollable"
        );
    }

    #[test]
    fn another_instances_pollable_handle_has_no_authority() {
        let (mut first, bindings) = resources(1);
        let handle = created(&mut first, &bindings);
        let (mut second, second_bindings) = resources(1);
        assert!(
            matches!(second.call(Request::Ready(handle), &second_bindings), Err(Error::InvalidHandle(received)) if received == handle),
            "another instance's pollable must not resolve in this instance"
        );
    }

    #[test]
    fn an_unready_poll_refuses_synchronous_waiting() {
        let (mut resources, bindings) = resources(1);
        let handle = resources
            .insert(Timer {
                ready: Arc::new(AtomicBool::new(false)),
                _task: None,
            })
            .expect("the unready test timer must fit its resource budget");
        assert!(
            matches!(
                resources.call(Request::Poll(vec![handle]), &bindings),
                Err(Error::Unsupported(_))
            ),
            "a poll with no ready resource must refuse synchronous waiting"
        );
    }

    #[test]
    fn polling_returns_only_the_indexes_of_ready_resources() {
        let (mut resources, bindings) = resources(2);
        let pending = resources
            .insert(Timer {
                ready: Arc::new(AtomicBool::new(false)),
                _task: None,
            })
            .expect("the pending test pollable must fit");
        let ready = created(&mut resources, &bindings);
        let Response::ReadyIndexes(indexes) = resources
            .call(Request::Poll(vec![pending, ready]), &bindings)
            .expect("polling with a ready resource must return")
        else {
            panic!("polling must return ready indexes")
        };
        assert_eq!(
            indexes,
            vec![1],
            "poll must report the ready input index without reporting pending resources"
        );
    }
}
