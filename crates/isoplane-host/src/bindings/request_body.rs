//! Upload ownership for one HTTP dispatch, with no guest-selectable source handle.

#[cfg(test)]
use std::num::NonZeroU32;
use std::sync::Arc;

use isoplane_abi::host::HttpBody;
use isoplane_abi::result::FutureError;
use parking_lot::Mutex;

struct Reader {
    body: Box<dyn HttpBody>,
    remaining: Vec<u8>,
    offset: usize,
}

impl Reader {
    async fn fill(&mut self) -> Result<bool, FutureError> {
        while self.offset == self.remaining.len() {
            self.remaining = Vec::new();
            self.offset = 0;
            match self.body.read().await {
                Ok(Some(bytes)) => {
                    self.remaining = bytes;
                    self.offset = 0;
                }
                Ok(None) => return Ok(false),
                Err(error) => return Err(FutureError::Io(format!("request upload: {error}"))),
            }
        }
        Ok(true)
    }

    fn read(&mut self, max_bytes: usize) -> Vec<u8> {
        let length = max_bytes.min(self.remaining.len() - self.offset);
        let bytes = self.remaining[self.offset..self.offset + length].to_vec();
        self.offset += length;
        bytes
    }
}

struct Open {
    reader: Option<Reader>,
    reading: bool,
    task: Option<tokio::task::AbortHandle>,
}
impl Drop for Open {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(crate) struct RequestBodyScope {
    can_have_body: bool,
    state: Mutex<Option<Open>>,
}

impl RequestBodyScope {
    pub(crate) fn new(body: Option<Box<dyn HttpBody>>, can_have_body: bool) -> Arc<Self> {
        Arc::new(Self {
            can_have_body,
            state: Mutex::new(Some(Open {
                reader: body.map(|body| Reader {
                    body,
                    remaining: vec![],
                    offset: 0,
                }),
                reading: false,
                task: None,
            })),
        })
    }

    pub(crate) fn close(&self) {
        self.state.lock().take();
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state
            .lock()
            .as_ref()
            .is_some_and(|state| state.reader.is_some() || state.reading)
    }

    pub(crate) fn can_have_body(&self) -> Result<bool, FutureError> {
        self.state.lock().as_ref().ok_or(FutureError::Cancelled)?;
        Ok(self.can_have_body)
    }

    pub(crate) fn begin_read(self: &Arc<Self>) -> Result<RequestRead, FutureError> {
        let mut guard = self.state.lock();
        let state = guard.as_mut().ok_or(FutureError::Cancelled)?;
        if state.reading {
            return Err(FutureError::Denied(
                "a request body read is already pending".into(),
            ));
        }
        state.reading = true;
        Ok(RequestRead {
            scope: self.clone(),
            reader: state.reader.take(),
            delivered: false,
            consumed: 0,
        })
    }

    pub(super) fn attach_task(&self, task: tokio::task::AbortHandle) {
        let mut state = self.state.lock();
        if let Some(state) = state.as_mut() {
            if state.reading {
                state.task = Some(task);
                return;
            }
        }
        task.abort();
    }
}

pub(crate) struct RequestRead {
    scope: Arc<RequestBodyScope>,
    reader: Option<Reader>,
    delivered: bool,
    consumed: usize,
}
impl RequestRead {
    pub(crate) async fn prepare(&mut self, max_bytes: usize) -> Result<usize, FutureError> {
        let result = match &mut self.reader {
            Some(reader) => reader.fill().await,
            None => Ok(false),
        };
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.reader = None;
            }
            Err(error) => {
                self.scope.close();
                self.reader = None;
                return Err(error);
            }
        }
        Ok(self.reader.as_ref().map_or(0, |reader| {
            max_bytes.min(reader.remaining.len() - reader.offset)
        }))
    }

    pub(crate) fn consume(&mut self, max_bytes: usize) -> Result<Vec<u8>, FutureError> {
        let bytes = self
            .reader
            .as_mut()
            .map_or_else(Vec::new, |reader| reader.read(max_bytes));
        self.consumed = bytes.len();
        if self.scope.state.lock().is_none() {
            return Err(FutureError::Cancelled);
        }
        Ok(bytes)
    }

    #[cfg(test)]
    async fn read_pending(&mut self, max_bytes: usize) -> Result<Vec<u8>, FutureError> {
        self.prepare(max_bytes).await?;
        self.consume(max_bytes)
    }

    #[cfg(test)]
    pub(crate) async fn read(mut self, max_bytes: NonZeroU32) -> Result<Vec<u8>, FutureError> {
        let bytes = self.read_pending(max_bytes.get() as usize).await?;
        self.delivered = true;
        Ok(bytes)
    }
}

impl crate::futures::ResultDelivery for RequestRead {
    fn delivered(&mut self) {
        self.delivered = true;
    }
}

impl Drop for RequestRead {
    fn drop(&mut self) {
        if !self.delivered {
            if let Some(reader) = &mut self.reader {
                reader.offset -= self.consumed;
            }
        }
        if let Some(state) = self.scope.state.lock().as_mut() {
            state.reader = self.reader.take();
            state.reading = false;
            state.task = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Chunks(VecDeque<Vec<u8>>);
    #[async_trait::async_trait]
    impl HttpBody for Chunks {
        async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self.0.pop_front())
        }
    }

    #[tokio::test]
    async fn partial_request_reads_preserve_frame_remainders_and_repeated_eof() {
        let scope = RequestBodyScope::new(
            Some(Box::new(Chunks(VecDeque::from([vec![], b"ABCD".to_vec()])))),
            true,
        );
        for expected in [b"AB".to_vec(), b"CD".to_vec(), vec![], vec![]] {
            let actual = scope
                .begin_read()
                .expect("one reader")
                .read(NonZeroU32::new(2).expect("positive read size"))
                .await
                .expect("upload read");
            assert_eq!(
                actual, expected,
                "read size must not discard frame bytes or confuse an empty data frame with EOF"
            );
        }
        assert!(
            !scope.is_active(),
            "EOF must release the client transport from capture-visible state"
        );
        assert!(
            scope.can_have_body().expect("live HTTP request"),
            "body-presence metadata must remain stable after consumption"
        );
    }

    #[tokio::test]
    async fn abandoned_reads_preserve_the_source_and_undelivered_bytes() {
        use crate::futures::FutureTable;
        use isoplane_abi::result::ResultRecord;
        let scope = RequestBodyScope::new(
            Some(Box::new(Chunks(VecDeque::from([b"ABCD".to_vec()])))),
            true,
        );
        drop(scope.begin_read().expect("first reader"));
        assert!(scope.is_active(), "abandoning a read preserves its upload");
        for byte_limit in [1, 8] {
            let table = FutureTable::new(byte_limit);
            let owner = crate::InstanceId(1);
            let future = table
                .alloc(owner, 1, "req_body_read", 1)
                .expect("allocation");
            let mut read = scope.begin_read().expect("retry reader");
            let bytes = read.read_pending(2).await.expect("read");
            assert_eq!(bytes, b"AB");
            table.complete_with_delivery(future, ResultRecord::Ok(bytes), Some(Box::new(read)));
            assert!(table.cancel_owned(owner, future.into()));
        }
        assert_eq!(
            scope
                .begin_read()
                .expect("retry")
                .read(NonZeroU32::new(4).expect("positive"))
                .await
                .expect("read"),
            b"ABCD"
        );
    }

    struct Gated {
        gate: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for Gated {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    #[async_trait::async_trait]
    impl HttpBody for Gated {
        async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
            self.gate.notified().await;
            Ok(Some(b"late".to_vec()))
        }
    }

    #[tokio::test]
    async fn a_late_upload_read_cannot_restore_transport_into_a_closed_dispatch() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let scope = RequestBodyScope::new(
            Some(Box::new(Gated {
                gate: gate.clone(),
                dropped: dropped.clone(),
            })),
            true,
        );
        let read = scope
            .begin_read()
            .expect("first reader")
            .read(NonZeroU32::new(8).expect("positive read size"));
        tokio::pin!(read);
        assert!(
            futures::poll!(&mut read).is_pending(),
            "the upload must await the controlled client"
        );
        assert!(
            scope.is_active(),
            "a pending read must remain visible to capture"
        );
        assert!(
            matches!(scope.begin_read(), Err(FutureError::Denied(_))),
            "a second reader must not steal an in-flight source"
        );
        scope.close();
        gate.notify_one();
        assert!(
            matches!(read.await, Err(FutureError::Cancelled)),
            "late completion must not revive a previous request"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "closed dispatch must release the upload transport"
        );
        assert!(
            !scope.is_active(),
            "late completion must not reinsert capture-visible state"
        );
    }

    #[tokio::test]
    async fn closing_a_request_scope_aborts_its_pending_upload_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let scope = RequestBodyScope::new(
            Some(Box::new(Gated {
                gate: Arc::new(tokio::sync::Notify::new()),
                dropped: dropped.clone(),
            })),
            true,
        );
        let read = scope.begin_read().expect("one reader");
        let task = tokio::spawn(read.read(NonZeroU32::new(1).expect("positive read size")));
        scope.attach_task(task.abort_handle());
        scope.close();
        assert!(
            task.await
                .expect_err("scope closure must abort the pending read")
                .is_cancelled(),
            "read task must have explicit cancellation ownership"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "aborting read must release the client transport"
        );
    }
    struct BrokenUpload(Arc<AtomicBool>);
    impl Drop for BrokenUpload {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    #[async_trait::async_trait]
    impl HttpBody for BrokenUpload {
        async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset).into())
        }
    }

    #[tokio::test]
    async fn a_transport_read_error_closes_upload_ownership_and_is_not_eof() {
        let dropped = Arc::new(AtomicBool::new(false));
        let source = RequestBodyScope::new(Some(Box::new(BrokenUpload(dropped.clone()))), true);
        let result = source
            .begin_read()
            .expect("open source")
            .read(NonZeroU32::new(1).expect("positive read"))
            .await;
        assert!(
            matches!(result, Err(FutureError::Io(_))),
            "a broken upload must produce an I/O failure instead of silently returning EOF"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "a failed upload read releases the transport immediately"
        );
        assert!(
            !source.is_active(),
            "a failed upload cannot remain an invisible capture veto"
        );
        assert!(
            matches!(source.begin_read(), Err(FutureError::Cancelled)),
            "a failed source cannot be revived by a later read"
        );
    }
}
