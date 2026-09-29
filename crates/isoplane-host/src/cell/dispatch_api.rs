//! The dispatch submission API: what a caller hands the cell and what comes back.

use super::*;

#[derive(Debug, Clone)]
pub enum Target {
    /// Any instance of the module.
    Handler { route: String },
}

impl Target {
    pub(super) fn route(&self) -> String {
        match self {
            Target::Handler { route } => route.clone(),
        }
    }
}

#[derive(Debug)]
pub struct DispatchResult {
    /// Echoed so a caller draining one channel for many dispatches can match
    /// a result to its request; completions arrive out of order.
    pub dispatch_id: u64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub completion: Completion,
    /// The recorded meter.
    pub cpu_ns: u64,
    /// Telemetry only.
    pub wall_ns: u64,
    pub trap: Option<String>,
    pub instance: InstanceId,
    /// How the dispatch got its instance; see `AcquireSource::label`.
    pub acquired: &'static str,
    /// One for a dispatch that never parked, more if it awaited.
    pub slices: u32,
}

impl DispatchResult {
    pub(super) fn shed(dispatch_id: u64) -> Self {
        Self {
            dispatch_id,
            status: 503,
            headers: vec![("retry-after".into(), "1".into())],
            body: b"cell shed load".to_vec(),
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some("shed".into()),
            instance: InstanceId(0),
            acquired: "none",
            slices: 0,
        }
    }
    fn breaker_open(dispatch_id: u64) -> Self {
        Self {
            dispatch_id,
            status: 503,
            headers: vec![],
            body: b"hang breaker open".to_vec(),
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some("breaker_open".into()),
            instance: InstanceId(0),
            acquired: "none",
            slices: 0,
        }
    }
}

/// Decided while the guard is still held, because it chooses between
/// releasing the guard and destroying it, and a guard released before its
/// tombstone lets another thread enter memory a trap abandoned. A latched trap
/// counts: `settle` promotes it to `Completion::Trapped`.
pub(super) fn ends_in_tombstone(out: &DispatchOutcome, slot: &Arc<Slot>) -> bool {
    matches!(out.completion, Completion::Trapped | Completion::BudgetKill)
        || (matches!(out.completion, Completion::Done | Completion::Parked) && slot.ctx.has_trap())
}

#[derive(Clone)]
pub(super) enum Responder {
    Blocking(std::sync::mpsc::SyncSender<DispatchResult>),
    Async(Arc<Mutex<Option<tokio::sync::oneshot::Sender<DispatchResult>>>>),
}

impl From<std::sync::mpsc::SyncSender<DispatchResult>> for Responder {
    fn from(sender: std::sync::mpsc::SyncSender<DispatchResult>) -> Self {
        Self::Blocking(sender)
    }
}

/// Sends a result to a caller that may have stopped waiting, which is not an
/// error in the cell.
pub(super) fn answer(tx: &Responder, result: DispatchResult) {
    let dispatch_id = result.dispatch_id;
    let disconnected = match tx {
        Responder::Blocking(tx) => tx.send(result).is_err(),
        Responder::Async(tx) => match tx.lock().take() {
            Some(tx) => tx.send(result).is_err(),
            None => return,
        },
    };
    if disconnected {
        tracing::debug!(
            dispatch_id,
            "the caller stopped waiting before its dispatch was answered"
        );
    }
}

impl Cell {
    /// Blocks until the response is final: a dispatch that parks and is later
    /// woken keeps this call outstanding.
    pub fn dispatch_blocking(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
    ) -> crate::Result<DispatchResult> {
        self.dispatch_with_stream(module, target, rec, timeout, None)
    }

    pub(crate) fn dispatch_with_stream(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
        stream: Option<Arc<crate::response_transport::ResponseTransport>>,
    ) -> crate::Result<DispatchResult> {
        self.dispatch_with_http(
            module,
            target,
            rec,
            timeout,
            stream.map(|response| super::HttpDispatch {
                response,
                upload: None,
            }),
        )
    }

    pub(crate) fn dispatch_with_http(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
        http: Option<super::HttpDispatch>,
    ) -> crate::Result<DispatchResult> {
        self.dispatch_with_resources(module, target, rec, timeout, http)
    }

    pub(crate) async fn dispatch_http_async(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
        http: super::HttpDispatch,
    ) -> crate::Result<DispatchResult> {
        self.dispatch_resources_async(module, target, rec, timeout, Some(http))
            .await
    }

    fn dispatch_with_resources(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
        resources: Option<super::HttpDispatch>,
    ) -> crate::Result<DispatchResult> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.enqueue_dispatch(module, target, rec, resources, tx.into())?;
        rx.recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("dispatch did not complete within {timeout:?}"))
    }

    fn enqueue_dispatch(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        resources: Option<super::HttpDispatch>,
        responder: Responder,
    ) -> crate::Result<()> {
        let http = resources;
        let (stream, upload) = match http {
            Some(http) => (Some(http.response), http.upload),
            None => (None, None),
        };
        let completion = self.completion_owner(module, &rec)?;
        let route = target.route();
        let now = crate::now_ms();
        let key = BreakerKey {
            module: module.clone(),
            route: route.clone(),
        };
        let dispatch_id = rec.dispatch_id;
        if self.breaker.check(&key, now).is_err() {
            completion.reject(super::completion::AdmissionRejection::BreakerOpen);
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            answer(&responder, DispatchResult::breaker_open(dispatch_id));
            return Ok(());
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder,
            stream,
            upload,
            completion,
        });
        if let Err(job) = self.queue.push_dispatch(job) {
            job.completion
                .reject(super::completion::AdmissionRejection::QueueUnavailable);
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            answer(&job.responder, DispatchResult::shed(dispatch_id));
        }
        Ok(())
    }

    async fn dispatch_resources_async(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
        resources: Option<super::HttpDispatch>,
    ) -> crate::Result<DispatchResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.enqueue_dispatch(
            module,
            target,
            rec,
            resources,
            Responder::Async(Arc::new(Mutex::new(Some(tx)))),
        )?;
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| anyhow::anyhow!("dispatch did not complete within {timeout:?}"))?
            .map_err(|_| anyhow::anyhow!("dispatch response channel closed before completion"))
    }

    /// Submits without waiting; `None` if the cell shed at admission.
    pub fn submit(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
    ) -> Option<std::sync::mpsc::Receiver<DispatchResult>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.submit_with(module, target, rec, tx).then_some(rx)
    }

    /// Submits onto a caller-provided channel that many submissions may
    /// share; results carry [`DispatchResult::dispatch_id`] for matching.
    /// Returns `false` if the cell shed at admission, and sends nothing in
    /// that case, so a caller draining the channel counts each shed once. A
    /// breaker rejection is sent, because it returns `true`. The sender is an
    /// execution-pool thread, so a channel the caller lets fill stops the
    /// cell.
    pub fn submit_with(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
    ) -> bool {
        let responder = Responder::Blocking(responder);
        let completion = match self.completion_owner(module, &rec) {
            Ok(completion) => completion,
            Err(error) => {
                tracing::warn!(module = %module, %error, "dispatch admission refused");
                return false;
            }
        };
        let now = crate::now_ms();
        let dispatch_id = rec.dispatch_id;
        let key = BreakerKey {
            module: module.clone(),
            route: target.route(),
        };
        if self.breaker.check(&key, now).is_err() {
            completion.reject(super::completion::AdmissionRejection::BreakerOpen);
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            answer(&responder, DispatchResult::breaker_open(dispatch_id));
            return true;
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder,
            stream: None,
            upload: None,
            completion,
        });
        match self.queue.push_dispatch(job) {
            Ok(()) => true,
            Err(job) => {
                job.completion
                    .reject(super::completion::AdmissionRejection::QueueUnavailable);
                self.metrics.shed.fetch_add(1, Ordering::Relaxed);
                if let Some(m) = self.meters(module) {
                    m.record_shed();
                }
                false
            }
        }
    }

    /// Async form, for callers already on a reactor.
    pub async fn dispatch(
        self: &Arc<Self>,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
    ) -> crate::Result<DispatchResult> {
        self.dispatch_resources_async(module, target, rec, timeout, None)
            .await
    }

    /// The completion path for an alarm, a socket frame, a DI reply or a
    /// test; the vtable's own futures come through the same runtime call.
    pub fn complete_future(&self, id: u64, result: isoplane_abi::result::ResultRecord) {
        self.rt.settle_future(id, result);
    }

    pub(crate) fn cpu_budget_ms(&self, module: &ModuleId) -> u32 {
        self.modules.read().get(module).map_or(30000, |m| {
            m.debug_cpu_ms
                .unwrap_or(m.manifest.effective_limits().cpu_ms_per_http_dispatch)
        })
    }

    pub fn http_record(&self, module: &ModuleId, req: isoplane_abi::HttpRequest) -> DispatchRecord {
        let cpu_budget_ms = self.cpu_budget_ms(module);
        DispatchRecord {
            dispatch_id: self.next_dispatch.fetch_add(1, Ordering::Relaxed),
            kind: DispatchKind::Http(req),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms,
            now_ms: crate::now_ms(),
        }
    }
}
