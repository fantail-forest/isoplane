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

/// Sends a result to a caller that may have stopped waiting, which is not an
/// error in the cell.
pub(super) fn answer(tx: &std::sync::mpsc::SyncSender<DispatchResult>, result: DispatchResult) {
    let dispatch_id = result.dispatch_id;
    if tx.send(result).is_err() {
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
        let enqueued_ns = crate::mono_ns();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let route = target.route();
        let now = crate::now_ms();
        let key = BreakerKey {
            module: module.clone(),
            route: route.clone(),
        };
        let dispatch_id = rec.dispatch_id;
        if self.breaker.check(&key, now).is_err() {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            return Ok(DispatchResult::breaker_open(dispatch_id));
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder: tx,
            enqueued_ns,
        });
        if self.queue.push_dispatch(job).is_err() {
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return Ok(DispatchResult::shed(dispatch_id));
        }
        rx.recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("dispatch did not complete within {timeout:?}"))
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
        let enqueued_ns = crate::mono_ns();
        let now = crate::now_ms();
        let dispatch_id = rec.dispatch_id;
        let key = BreakerKey {
            module: module.clone(),
            route: target.route(),
        };
        if self.breaker.check(&key, now).is_err() {
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
            enqueued_ns,
        });
        match self.queue.push_dispatch(job) {
            Ok(()) => true,
            Err(_) => {
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
        let me = self.clone();
        let module = module.clone();
        tokio::task::spawn_blocking(move || me.dispatch_blocking(&module, target, rec, timeout))
            .await?
    }

    /// The completion path for an alarm, a socket frame, a DI reply or a
    /// test; the vtable's own futures come through the same runtime call.
    pub fn complete_future(&self, id: u64, result: wisp_abi::result::ResultRecord) {
        self.rt.settle_future(id, result);
    }

    pub fn http_record(&self, module: &ModuleId, req: wisp_abi::HttpRequest) -> DispatchRecord {
        let limits = self
            .modules
            .read()
            .get(module)
            .map_or(Limits::default(), |m| m.limits);
        DispatchRecord {
            dispatch_id: self.next_dispatch.fetch_add(1, Ordering::Relaxed),
            kind: DispatchKind::Http(req),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: limits.cpu_ms_per_http_dispatch,
            now_ms: crate::now_ms(),
        }
    }
}
