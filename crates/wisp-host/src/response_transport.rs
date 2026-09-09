//! Response ownership between one dispatch and its requester.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;
use wisp_abi::dispatch::ResponseHead;
use wisp_abi::host::{HttpBody, HttpExchange, HttpResponseBody, ResponseDisposition};
use wisp_abi::result::FutureError;

#[derive(Default)]
struct State {
    head: Option<(u16, Vec<(String, String)>)>,
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    remaining_bytes: Option<u64>,
    pending_bytes: usize,
    queued_peak_bytes: usize,
    pending_peak_bytes: usize,
    finished: bool,
    closing: bool,
    write_turn: u64,
    cancelled: bool,
    disposition: Option<ResponseDisposition>,
    settled: bool,
    failure: Option<String>,
    initial_write: Option<tokio::task::AbortHandle>,
}

pub(crate) struct ResponseTransport {
    state: Mutex<State>,
    queue_bytes: usize,
    next_write: AtomicU64,
    readable: Notify,
    writable: Notify,
    cancellation: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl ResponseTransport {
    pub(crate) fn new(queue_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            queue_bytes,
            next_write: AtomicU64::new(0),
            readable: Notify::new(),
            writable: Notify::new(),
            cancellation: OnceLock::new(),
        })
    }

    pub(crate) fn on_end_requested(&self, callback: Box<dyn Fn() + Send + Sync>) {
        if self.cancellation.set(callback).is_err() {
            tracing::error!("response cancellation owner was assigned twice");
        }
        if self.end_requested() {
            if let Some(callback) = self.cancellation.get() {
                callback();
            }
        }
    }

    fn end_requested(&self) -> bool {
        let state = self.state.lock();
        !state.settled
            && (state.cancelled || state.failure.is_some() || state.disposition.is_some())
    }

    pub(crate) fn no_body_head(&self) -> Option<(u16, Vec<(String, String)>)> {
        let state = self.state.lock();
        state.disposition.and_then(|_| state.head.clone())
    }

    pub(crate) fn has_owner(&self) -> bool {
        self.cancellation.get().is_some()
    }

    pub(crate) fn settlement(self: Arc<Self>) -> ResponseSettlement {
        ResponseSettlement(self)
    }

    pub(crate) fn acknowledge_settlement(&self) {
        self.state.lock().settled = true;
        self.readable.notify_waiters();
    }

    fn request_disposition(&self, disposition: ResponseDisposition) {
        let mut state = self.state.lock();
        let notify_owner = !state.settled;
        state.disposition = Some(disposition);
        state.chunks.clear();
        state.queued_bytes = 0;
        if let Some(task) = state.initial_write.take() {
            task.abort();
        }
        drop(state);
        self.writable.notify_waiters();
        if notify_owner {
            if let Some(callback) = self.cancellation.get() {
                callback();
            }
        }
    }

    async fn await_settlement(&self) -> anyhow::Result<()> {
        loop {
            let changed = self.readable.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock();
                if state.settled {
                    if let Some(error) = &state.failure {
                        anyhow::bail!("{error}");
                    }
                    anyhow::ensure!(!state.cancelled, "response requester cancelled");
                    return Ok(());
                }
            }
            changed.await;
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.lock().cancelled
    }

    pub(crate) fn start(&self, head: &ResponseHead) -> Result<(), FutureError> {
        let mut state = self.state.lock();
        if state.cancelled {
            return Err(FutureError::Cancelled);
        }
        if state.head.is_some() || state.finished {
            return Err(FutureError::Denied("a response has already started".into()));
        }
        state.head = Some((head.status, head.headers.clone()));
        state.remaining_bytes = (!head.streaming).then_some(head.body.len() as u64);
        drop(state);
        self.readable.notify_waiters();
        Ok(())
    }

    pub(crate) fn own_initial_write(&self, task: tokio::task::AbortHandle) {
        let mut state = self.state.lock();
        if state.cancelled || state.failure.is_some() || state.disposition.is_some() {
            task.abort();
        } else {
            state.initial_write = Some(task);
        }
    }

    pub(crate) fn finish(&self) {
        let mut state = self.state.lock();
        state.closing = true;
        state.finished = state.disposition.is_some() || state.pending_bytes == 0;
        drop(state);
        self.readable.notify_waiters();
        self.writable.notify_waiters();
    }

    fn fail_locked(state: &mut State, error: String) -> bool {
        if state.failure.is_none() {
            state.failure = Some(error);
        }
        state.finished = true;
        if let Some(task) = state.initial_write.take() {
            task.abort();
        }
        !state.settled
    }

    fn notify_change(&self, notify_owner: bool) {
        self.readable.notify_waiters();
        self.writable.notify_waiters();
        if notify_owner {
            if let Some(callback) = self.cancellation.get() {
                callback();
            }
        }
    }

    pub(crate) fn fail(&self, error: String) {
        let notify_owner = Self::fail_locked(&mut self.state.lock(), error);
        self.notify_change(notify_owner);
    }

    fn cancel(&self) {
        let mut state = self.state.lock();
        if state.cancelled {
            return;
        }
        let notify_owner = !state.settled && (!state.finished || state.disposition.is_some());
        state.cancelled = true;
        if let Some(task) = state.initial_write.take() {
            task.abort();
        }
        state.chunks.clear();
        state.queued_bytes = 0;
        drop(state);
        self.readable.notify_waiters();
        self.writable.notify_waiters();
        if notify_owner {
            if let Some(callback) = self.cancellation.get() {
                callback();
            }
        }
    }

    pub(crate) async fn exchange(self: &Arc<Self>) -> anyhow::Result<HttpExchange> {
        let reader = StreamingBody {
            transport: self.clone(),
        };
        loop {
            let changed = self.readable.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock();
                if let Some((status, headers)) = &state.head {
                    return Ok(HttpExchange {
                        status: *status,
                        headers: headers.clone(),
                        body: Box::new(reader),
                    });
                }
                if let Some(error) = &state.failure {
                    anyhow::bail!("{error}");
                }
                anyhow::ensure!(
                    !state.cancelled,
                    "request cancelled before response headers"
                );
            }
            changed.await;
        }
    }

    pub(crate) fn prepare(self: &Arc<Self>, bytes: Vec<u8>) -> PendingWrite {
        PendingWrite::new(self.clone(), bytes)
    }

    pub(crate) fn record_buffers(&self, dispatch_id: u64) {
        let state = self.state.lock();
        tracing::debug!(
            dispatch_id,
            response_queued_peak_bytes = state.queued_peak_bytes,
            response_pending_write_peak_bytes = state.pending_peak_bytes,
            response_queued_bytes = state.queued_bytes,
            response_pending_write_bytes = state.pending_bytes,
            "HTTP response buffer ownership"
        );
    }

    pub(crate) fn has_started(&self) -> bool {
        self.state.lock().head.is_some()
    }
    pub(crate) fn failure(&self) -> Option<String> {
        self.state.lock().failure.clone()
    }

    async fn write(self: &Arc<Self>, mut pending: PendingWrite) -> Result<usize, FutureError> {
        let count = pending.bytes.len();
        let mut offset = 0;
        loop {
            let changed = self.writable.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state.lock();
                if state.cancelled {
                    return Err(FutureError::Cancelled);
                }
                if state.disposition.is_some() {
                    break;
                }
                if state.finished {
                    return Err(FutureError::Io("response already finished".into()));
                }
                if state.write_turn == pending.ticket && offset == count {
                    break;
                }
                let available = if state.write_turn == pending.ticket {
                    self.queue_bytes.saturating_sub(state.queued_bytes)
                } else {
                    0
                };
                let length = available.min(count - offset);
                if length != 0 {
                    state
                        .chunks
                        .push_back(pending.bytes[offset..offset + length].to_vec());
                    state.queued_bytes += length;
                    state.queued_peak_bytes = state.queued_peak_bytes.max(state.queued_bytes);
                    offset += length;
                    self.readable.notify_waiters();
                }
            }
            if offset == count && self.state.lock().write_turn == pending.ticket {
                break;
            }
            changed.await;
        }
        pending.completed = true;
        self.state.lock().write_turn += 1;
        self.writable.notify_waiters();
        Ok(count)
    }
}

pub(crate) struct PendingWrite {
    transport: Arc<ResponseTransport>,
    bytes: Vec<u8>,
    ticket: u64,
    completed: bool,
}
impl PendingWrite {
    fn new(transport: Arc<ResponseTransport>, bytes: Vec<u8>) -> Self {
        {
            let mut state = transport.state.lock();
            state.pending_bytes += bytes.len();
            state.pending_peak_bytes = state.pending_peak_bytes.max(state.pending_bytes);
        }
        let ticket = transport.next_write.fetch_add(1, Ordering::Relaxed);
        Self {
            transport,
            bytes,
            ticket,
            completed: false,
        }
    }

    pub(crate) async fn send(self) -> Result<usize, FutureError> {
        let transport = self.transport.clone();
        transport.write(self).await
    }
}
impl Drop for PendingWrite {
    fn drop(&mut self) {
        let notify_owner = {
            let mut state = self.transport.state.lock();
            state.pending_bytes -= self.bytes.len();
            if state.closing && state.pending_bytes == 0 {
                state.finished = true;
            }
            if !self.completed && !state.cancelled && state.disposition.is_none() {
                ResponseTransport::fail_locked(
                    &mut state,
                    "a response write was cancelled before acknowledgement".into(),
                )
            } else {
                false
            }
        };
        self.transport.notify_change(notify_owner);
    }
}

pub(crate) struct ResponseSettlement(Arc<ResponseTransport>);

impl Drop for ResponseSettlement {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.fail("host panic during response cleanup".into());
        }
        self.0.acknowledge_settlement();
    }
}

struct StreamingBody {
    transport: Arc<ResponseTransport>,
}
#[async_trait]
impl HttpBody for StreamingBody {
    fn remaining_bytes(&self) -> Option<u64> {
        self.transport.state.lock().remaining_bytes
    }

    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        loop {
            let changed = self.transport.readable.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.transport.state.lock();
                if let Some(bytes) = state.chunks.pop_front() {
                    state.queued_bytes -= bytes.len();
                    if let Some(remaining) = &mut state.remaining_bytes {
                        *remaining -= bytes.len() as u64;
                    }
                    self.transport.writable.notify_waiters();
                    return Ok(Some(bytes));
                }
                if let Some(error) = &state.failure {
                    anyhow::bail!("{error}");
                }
                anyhow::ensure!(!state.cancelled, "response cancelled");
                if state.finished {
                    return Ok(None);
                }
            }
            changed.await;
        }
    }
}
#[async_trait]
impl HttpResponseBody for StreamingBody {
    async fn finish(self: Box<Self>, disposition: ResponseDisposition) -> anyhow::Result<()> {
        self.transport.request_disposition(disposition);
        self.transport.await_settlement().await
    }
}

impl Drop for StreamingBody {
    fn drop(&mut self) {
        self.transport.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(queue_bytes: usize) -> Arc<ResponseTransport> {
        let stream = ResponseTransport::new(queue_bytes);
        stream
            .start(&ResponseHead {
                status: 200,
                headers: vec![],
                body: vec![],
                streaming: true,
            })
            .expect("the fixture response must start");
        stream
    }

    #[tokio::test]
    async fn no_body_completion_waits_for_terminal_cleanup_without_consuming_bytes() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let mut write = Box::pin(stream.prepare(b"suppressed".to_vec()).send());
        assert!(
            futures::poll!(&mut write).is_pending(),
            "the original body must be blocked by queue capacity"
        );
        let mut finish = Box::pin(response.body.finish(ResponseDisposition::NoBody));
        assert!(
            futures::poll!(&mut finish).is_pending(),
            "framing must wait for the host's terminal boundary"
        );
        assert_eq!(
            write.await.expect("suppressed write acknowledgement"),
            10,
            "body suppression accepts the original write without transmitting it"
        );
        assert_eq!(
            stream.state.lock().queued_bytes,
            0,
            "no-body framing must release queued wire data"
        );
        stream.finish();
        assert!(
            futures::poll!(&mut finish).is_pending(),
            "guest completion alone must not precede dispatch cleanup"
        );
        drop(stream.clone().settlement());
        finish.await.expect("bodyless dispatch completed normally");
        assert!(
            stream.failure().is_none(),
            "discarding protocol-forbidden bytes must not become a response write failure"
        );
    }

    #[tokio::test]
    async fn dropping_a_no_body_waiter_still_cancels_the_requester() {
        let stream = stream(1);
        let signals = Arc::new(AtomicU64::new(0));
        let observed = signals.clone();
        stream.on_end_requested(Box::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        }));
        let response = stream.exchange().await.expect("response headers");
        let mut finish = Box::pin(response.body.finish(ResponseDisposition::NoBody));
        assert!(
            futures::poll!(&mut finish).is_pending(),
            "the terminal acknowledgement must still be outstanding"
        );
        stream.finish();
        drop(finish);
        assert!(
            stream.is_cancelled(),
            "dropping the framing waiter must preserve generic requester cancellation"
        );
        assert_eq!(
            signals.load(Ordering::Relaxed),
            2,
            "a waiter dropped during cleanup must notify its owner even after response finish"
        );
    }

    #[tokio::test]
    async fn a_trap_before_no_body_acknowledgement_remains_a_failure() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let mut finish = Box::pin(response.body.finish(ResponseDisposition::NoBody));
        assert!(
            futures::poll!(&mut finish).is_pending(),
            "the guest slice must still be pending"
        );
        stream.fail("same-slice trap".into());
        assert!(
            futures::poll!(&mut finish).is_pending(),
            "a failed slice must also await its cleanup boundary"
        );
        drop(stream.clone().settlement());
        assert!(
            finish.await.is_err(),
            "a trap must not be relabelled successful no-body completion"
        );
    }

    #[tokio::test]
    async fn finishing_a_retained_settled_body_does_not_notify_its_old_owner() {
        let stream = stream(1);
        let signals = Arc::new(AtomicU64::new(0));
        let observed = signals.clone();
        stream.on_end_requested(Box::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        }));
        let response = stream.exchange().await.expect("response headers");
        let mut write = Box::pin(stream.prepare(b"old-body".to_vec()).send());
        assert!(
            futures::poll!(&mut write).is_pending(),
            "the completed guest may still have a buffered body awaiting its reader"
        );
        stream.finish();
        drop(stream.clone().settlement());
        response
            .body
            .finish(ResponseDisposition::NoBody)
            .await
            .expect("already settled response");
        drop(write);
        stream.fail("late writer failure".into());
        assert_eq!(
            signals.load(Ordering::Relaxed),
            0,
            "late body disposition, drop and writer failure must not wake a reused instance"
        );
        assert_eq!(
            stream.state.lock().queued_bytes,
            0,
            "the old body's queued bytes must still be released"
        );
        assert_eq!(
            stream.state.lock().pending_bytes,
            0,
            "the old body's pending write must still be released"
        );
    }

    #[tokio::test]
    async fn an_already_settled_failure_does_not_strand_a_no_body_waiter() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        stream.fail("earlier trap".into());
        drop(stream.clone().settlement());
        let mut finish = Box::pin(response.body.finish(ResponseDisposition::NoBody));
        assert!(
            matches!(futures::poll!(&mut finish), std::task::Poll::Ready(Err(_))),
            "framing after a terminal failure must receive that outcome immediately"
        );
    }

    #[tokio::test]
    async fn a_write_waits_for_queue_capacity_and_acknowledges_its_entire_payload() {
        let stream = stream(4);
        let mut response = stream
            .exchange()
            .await
            .expect("response headers must be published");
        let write = stream.prepare(b"abcdefgh".to_vec()).send();
        tokio::pin!(write);
        assert!(
            futures::poll!(&mut write).is_pending(),
            "a full transport queue must park the writer"
        );
        assert_eq!(
            stream.state.lock().queued_bytes,
            4,
            "queued bytes must obey the configured high-water mark"
        );
        assert_eq!(
            response.body.read().await.expect("first read"),
            Some(b"abcd".to_vec()),
            "the requester must receive the first segment before write acknowledgement"
        );
        assert_eq!(
            write.await.expect("full write"),
            8,
            "the acknowledgement must cover the whole submitted write"
        );
        stream.finish();
        assert_eq!(
            response.body.read().await.expect("second read"),
            Some(b"efgh".to_vec()),
            "finish must retain queued response bytes"
        );
        assert_eq!(
            response.body.read().await.expect("EOF"),
            None,
            "finished responses must end after their last byte"
        );
    }

    #[tokio::test]
    async fn cancelling_an_earlier_write_fails_later_tickets_instead_of_stranding_them() {
        let stream = stream(1);
        let mut first = Box::pin(stream.prepare(b"first".to_vec()).send());
        let mut second = Box::pin(stream.prepare(b"second".to_vec()).send());
        assert!(
            futures::poll!(&mut first).is_pending(),
            "the first write must fill the queue"
        );
        assert!(
            futures::poll!(&mut second).is_pending(),
            "the next write must await its predecessor"
        );
        drop(first);
        assert!(
            matches!(futures::poll!(&mut second), std::task::Poll::Ready(Err(_))),
            "a cancelled predecessor must fail the response rather than strand the next ticket"
        );
        assert_eq!(
            stream.state.lock().pending_bytes,
            0,
            "cancelled write payloads must release their ownership"
        );
    }

    #[tokio::test]
    async fn requester_cancellation_before_writer_disposal_stays_cancellation() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let pending = stream.prepare(b"pending".to_vec());
        drop(response);
        drop(pending);
        assert!(
            stream.is_cancelled(),
            "requester loss must cancel its transport"
        );
        assert!(
            stream.failure().is_none(),
            "disposing cancelled work must not replace requester loss with writer failure"
        );
        assert_eq!(
            stream.state.lock().pending_bytes,
            0,
            "cancelled payload ownership must be released"
        );
    }

    #[tokio::test]
    async fn writer_abandonment_before_requester_loss_retains_the_failure() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let pending = stream.prepare(b"pending".to_vec());
        drop(pending);
        let failure = stream.failure();
        assert!(
            failure.is_some(),
            "abandoning an active write must fail its response"
        );
        drop(response);
        assert_eq!(
            stream.failure(),
            failure,
            "later requester loss must preserve the existing writer failure"
        );
    }

    #[tokio::test]
    async fn dropping_the_response_releases_a_blocked_write_payload() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let mut write = Box::pin(stream.prepare(b"blocked".to_vec()).send());
        assert!(
            futures::poll!(&mut write).is_pending(),
            "the write must be blocked by the unread queue"
        );
        drop(response);
        assert!(
            matches!(
                futures::poll!(&mut write),
                std::task::Poll::Ready(Err(FutureError::Cancelled))
            ),
            "dropping a response must cancel its blocked writer"
        );
        assert_eq!(
            stream.state.lock().pending_bytes,
            0,
            "body drop must release pending payload ownership"
        );
        assert_eq!(
            stream.state.lock().queued_bytes,
            0,
            "body drop must release queued payload ownership"
        );
    }

    #[tokio::test]
    async fn finishing_preserves_an_initial_body_that_is_still_waiting_for_capacity() {
        let stream = stream(1);
        let mut response = stream.exchange().await.expect("response headers");
        let mut initial = Box::pin(stream.prepare(b"AB".to_vec()).send());
        assert!(
            futures::poll!(&mut initial).is_pending(),
            "the initial body must exceed the queue"
        );
        stream.finish();
        assert_eq!(
            response.body.read().await.expect("initial chunk"),
            Some(b"A".to_vec()),
            "finish must not discard an initial body"
        );
        assert_eq!(
            initial.await.expect("initial body drain"),
            2,
            "the whole initial body must be queued"
        );
        assert_eq!(
            response.body.read().await.expect("final chunk"),
            Some(b"B".to_vec()),
            "the remaining initial body must arrive"
        );
        assert_eq!(
            response.body.read().await.expect("EOF"),
            None,
            "initial body completion must release EOF"
        );
    }
    #[tokio::test]
    async fn dropping_a_failed_body_clears_queued_bytes_and_aborts_its_initial_writer() {
        let stream = stream(1);
        let response = stream.exchange().await.expect("response headers");
        let mut initial = Box::pin(stream.prepare(b"AB".to_vec()).send());
        assert!(
            futures::poll!(&mut initial).is_pending(),
            "the initial payload must await queue space"
        );
        let task = tokio::spawn(initial);
        stream.own_initial_write(task.abort_handle());
        stream.fail("guest trapped after headers".into());
        drop(response);
        assert!(
            task.await
                .expect_err("the failed transport must abort its owned task")
                .is_cancelled(),
            "initial writer cancellation must be explicit"
        );
        let state = stream.state.lock();
        assert_eq!(
            state.pending_bytes, 0,
            "failure must release pending payload ownership"
        );
        assert_eq!(
            state.queued_bytes, 0,
            "dropping a failed body must release queued ownership"
        );
    }
}
