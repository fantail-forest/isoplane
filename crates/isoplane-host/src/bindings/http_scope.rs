//! Dispatch ownership for outbound HTTP operations and their retained bodies.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use isoplane_abi::result::{BodyChunk, FetchResponse, FutureError};
use parking_lot::Mutex;

/// An outbound HTTP response before its body is assigned a guest handle.
pub struct OutboundResponse {
    /// The upstream response status.
    pub status: u16,
    /// The upstream response headers.
    pub headers: Vec<(String, String)>,
    /// Inline bytes or an owned streaming body.
    pub body: OutboundBody,
}

/// Body storage owned by the caller of the HTTP transport.
pub enum OutboundBody {
    /// A fully collected response.
    Inline(Vec<u8>),
    /// A response whose transport, and its open connection permit, is released
    /// when this object is dropped.
    Stream(Box<dyn ResponseBody>),
}

/// Reads an owned HTTP response without exposing the transport implementation.
#[async_trait::async_trait]
pub trait ResponseBody: Send + 'static {
    /// Reads at most `max_len` bytes, or the next available chunk when zero.
    /// EOF and transport errors end the body.
    async fn read(&mut self, max_len: usize) -> Result<BodyChunk, FutureError>;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BodyHandle(u32);

// Guest memory can retain handles across dispatches. Within this process,
// scopes share an allocator so stale handles never alias a new response.
static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

#[derive(Default)]
struct Resource {
    body: Option<Box<dyn ResponseBody>>,
    task: Option<tokio::task::AbortHandle>,
}

impl Drop for Resource {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(super) struct HttpScope {
    resources: Mutex<Option<HashMap<BodyHandle, Resource>>>,
}

impl HttpScope {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            resources: Mutex::new(Some(HashMap::new())),
        })
    }

    pub(super) fn close(&self) {
        self.resources.lock().take();
    }

    pub(super) fn is_active(&self) -> bool {
        self.resources
            .lock()
            .as_ref()
            .is_some_and(|resources| !resources.is_empty())
    }

    pub(super) fn begin_fetch(self: &Arc<Self>) -> Result<Operation, FutureError> {
        let mut state = self.resources.lock();
        let resources = state.as_mut().ok_or(FutureError::Cancelled)?;
        let handle = allocate_handle(&NEXT_HANDLE)?;
        resources.insert(handle, Resource::default());
        Ok(Operation {
            scope: self.clone(),
            handle,
            retained: false,
        })
    }

    pub(super) fn begin_read(
        self: &Arc<Self>,
        handle: u32,
    ) -> Result<(Operation, Box<dyn ResponseBody>), FutureError> {
        let handle = BodyHandle(handle);
        let mut state = self.resources.lock();
        let resource = state
            .as_mut()
            .and_then(|resources| resources.get_mut(&handle))
            .ok_or(FutureError::NotFound)?;
        let body = resource.body.take().ok_or(FutureError::NotFound)?;
        Ok((
            Operation {
                scope: self.clone(),
                handle,
                retained: false,
            },
            body,
        ))
    }

    pub(super) fn attach_task(&self, handle: BodyHandle, task: tokio::task::AbortHandle) {
        let mut state = self.resources.lock();
        if let Some(resource) = state
            .as_mut()
            .and_then(|resources| resources.get_mut(&handle))
        {
            if resource.body.is_none() {
                resource.task = Some(task);
                return;
            }
        }
        task.abort();
    }
}

pub(super) struct Operation {
    scope: Arc<HttpScope>,
    handle: BodyHandle,
    retained: bool,
}

impl Operation {
    pub(super) fn handle(&self) -> BodyHandle {
        self.handle
    }

    pub(super) fn response(
        mut self,
        response: OutboundResponse,
    ) -> Result<FetchResponse, FutureError> {
        let (body_handle, body_inline) = match response.body {
            OutboundBody::Inline(bytes) => (0, Some(bytes)),
            OutboundBody::Stream(body) => {
                self.retain(body)?;
                (self.handle.0, None)
            }
        };
        Ok(FetchResponse {
            status: response.status,
            headers: response.headers,
            body_handle,
            body_inline,
        })
    }

    pub(super) async fn read(
        mut self,
        mut body: Box<dyn ResponseBody>,
        max_len: usize,
    ) -> Result<BodyChunk, FutureError> {
        let chunk = body.read(max_len).await?;
        if !chunk.eof {
            self.retain(body)?;
        }
        Ok(chunk)
    }

    fn retain(&mut self, body: Box<dyn ResponseBody>) -> Result<(), FutureError> {
        let mut state = self.scope.resources.lock();
        let resource = state
            .as_mut()
            .and_then(|resources| resources.get_mut(&self.handle))
            .ok_or(FutureError::Cancelled)?;
        resource.body = Some(body);
        resource.task = None;
        self.retained = true;
        Ok(())
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.retained {
            if let Some(resources) = self.scope.resources.lock().as_mut() {
                if let Some(mut resource) = resources.remove(&self.handle) {
                    // This operation is already completing or being cancelled.
                    resource.task = None;
                }
            }
        }
    }
}

fn allocate_handle(next: &AtomicU32) -> Result<BodyHandle, FutureError> {
    next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        value.checked_add(1)
    })
    .map(BodyHandle)
    .map_err(|_| FutureError::Io("outbound HTTP body handle space exhausted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct GatedBody {
        gate: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for GatedBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    #[async_trait::async_trait]
    impl ResponseBody for GatedBody {
        async fn read(&mut self, _: usize) -> Result<BodyChunk, FutureError> {
            self.gate.notified().await;
            Ok(BodyChunk {
                data: b"late".to_vec(),
                eof: false,
            })
        }
    }

    #[test]
    fn exhausting_body_handles_cannot_wrap_into_a_previous_scope() {
        let next = AtomicU32::new(u32::MAX - 1);
        assert_eq!(
            allocate_handle(&next)
                .expect("the last available handle must allocate")
                .0,
            u32::MAX - 1,
            "allocation must preserve the final unused handle"
        );
        assert!(
            matches!(allocate_handle(&next), Err(FutureError::Io(_))),
            "handle exhaustion must fail rather than wrap into another scope"
        );
        assert_eq!(
            next.load(Ordering::Relaxed),
            u32::MAX,
            "exhaustion must leave the allocator permanently exhausted"
        );
    }

    #[tokio::test]
    async fn a_fetch_completing_after_its_scope_closed_drops_its_response() {
        let scope = HttpScope::new();
        let operation = scope
            .begin_fetch()
            .expect("the dispatch must register its fetch");
        scope.close();
        let dropped = Arc::new(AtomicBool::new(false));
        let result = operation.response(OutboundResponse {
            status: 200,
            headers: vec![],
            body: OutboundBody::Stream(Box::new(GatedBody {
                gate: Arc::new(tokio::sync::Notify::new()),
                dropped: dropped.clone(),
            })),
        });
        assert!(
            matches!(result, Err(FutureError::Cancelled)),
            "late fetch completion must not publish into a closed dispatch"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "the refused late response must drop its body, and its connection with it"
        );
        assert!(
            !scope.is_active(),
            "late completion must not recreate scoped resources"
        );
    }

    #[tokio::test]
    async fn a_read_completing_after_scope_closure_cannot_restore_its_body() {
        let scope = HttpScope::new();
        let fetch = scope
            .begin_fetch()
            .expect("the dispatch must register a fetch");
        let gate = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let response = fetch
            .response(OutboundResponse {
                status: 200,
                headers: vec![],
                body: OutboundBody::Stream(Box::new(GatedBody {
                    gate: gate.clone(),
                    dropped: dropped.clone(),
                })),
            })
            .expect("the response must be retained");
        let (read, body) = scope
            .begin_read(response.body_handle)
            .expect("the body must be readable");
        let pending = read.read(body, 8);
        tokio::pin!(pending);
        assert!(
            futures::poll!(&mut pending).is_pending(),
            "the controlled read must hold the body in flight"
        );
        assert!(
            scope.is_active(),
            "an in-flight read must remain visible to capture"
        );
        scope.close();
        assert!(
            !dropped.load(Ordering::Acquire),
            "scope invalidation must not pretend an in-flight body is already disposed"
        );
        gate.notify_one();
        assert!(
            matches!(pending.await, Err(FutureError::Cancelled)),
            "late reads must not restore bodies into closed scopes"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "the late body must be disposed"
        );
        assert!(
            !scope.is_active(),
            "late reads must not recreate capture-visible work"
        );
    }
}
