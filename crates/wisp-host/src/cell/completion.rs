//! One attributed dispatch owner, retained across queueing and parked intervals.

use super::*;

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

struct DispatchToken(u64);

impl DispatchToken {
    fn next(counter: &AtomicU64) -> crate::Result<Self> {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map(Self)
            .map_err(|_| anyhow::anyhow!("dispatch correlation identifiers exhausted"))
    }
}

pub(super) enum AdmissionRejection {
    QueueUnavailable,
    BreakerOpen,
}

pub(super) type CompletionClock = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

#[derive(Clone, Copy)]
pub(super) enum TerminalReason {
    Success,
    GuestTrap,
    CpuLimit,
    WallLimit,
    RequestCancelled,
    TransportFailure,
    AcquisitionFailed,
    SnapshotCapacityUnavailable,
    EntryRefused,
    RestoreFailed,
    CaptureFailed,
    HostPanic,
    CpuClockUnavailable,
    CellStopped,
}

impl TerminalReason {
    fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::GuestTrap => "guest_trap",
            Self::CpuLimit => "cpu_limit",
            Self::WallLimit => "execution_wall_limit",
            Self::RequestCancelled => "request_cancelled",
            Self::TransportFailure => "transport_failure",
            Self::AcquisitionFailed => "acquisition_failed",
            Self::SnapshotCapacityUnavailable => "snapshot_capacity_unavailable",
            Self::EntryRefused => "entry_refused",
            Self::RestoreFailed => "restore_failed",
            Self::CaptureFailed => "capture_failed",
            Self::HostPanic => "host_panic",
            Self::CpuClockUnavailable => "cpu_clock_unavailable",
            Self::CellStopped => "cell_stopped",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Usage {
    pub(super) cpu_ns: u64,
    pub(super) hot_wall_ns: u64,
    pub(super) slices: u32,
    incomplete: bool,
    invoking: bool,
    overage_ns: u64,
    limit: Option<wisp_engine::ExecutionLimit>,
    kill_phase: Option<wisp_engine::KillPhase>,
    capture_hint_cpu_ns: u64,
    capture_hint_wall_ns: u64,
    capture_hints: u32,
    hint_incomplete: bool,
    hint_running: bool,
    trap_kind: Option<wisp_engine::TrapKind>,
}

struct State {
    instance: Option<InstanceId>,
    acquired: Option<&'static str>,
    usage: Usage,
    outcome: Option<(TerminalReason, u16)>,
    emitted: bool,
    responded: bool,
    io_kind: Option<std::io::ErrorKind>,
    os_error: Option<i32>,
    guest_trap_code: Option<TrapCode>,
}

pub(super) struct DispatchCompletion {
    token: DispatchToken,
    pub(super) entry: Arc<ModuleEntry>,
    tenant: String,
    module: ModuleId,
    dispatch_id: u64,
    kind: &'static str,
    budget: CpuBudget,
    started_ns: Option<u64>,
    clock: CompletionClock,
    span: tracing::Span,
    subscriber: tracing::Dispatch,
    state: Mutex<State>,
}

impl DispatchCompletion {
    pub(super) fn new(
        entry: Arc<ModuleEntry>,
        record: &DispatchRecord,
        budget: CpuBudget,
        clock: CompletionClock,
    ) -> crate::Result<Arc<Self>> {
        let token = DispatchToken::next(&NEXT_TOKEN)?;
        let tenant = entry.tenant.clone();
        let module = entry.activator.module().clone();
        let span = tracing::info_span!("dispatch", tenant = %tenant, app = %module.app,
            version = %module.version, dispatch_id = record.dispatch_id, dispatch_token = token.0,
            dispatch_kind = record.kind.label(), instance = tracing::field::Empty);
        Ok(Arc::new(Self {
            token,
            entry,
            tenant,
            module,
            dispatch_id: record.dispatch_id,
            kind: record.kind.label(),
            budget,
            started_ns: clock(),
            clock,
            span,
            subscriber: tracing::dispatcher::get_default(Clone::clone),
            state: Mutex::new(State {
                instance: None,
                acquired: None,
                usage: Usage::default(),
                outcome: None,
                emitted: false,
                responded: false,
                io_kind: None,
                os_error: None,
                guest_trap_code: None,
            }),
        }))
    }

    pub(super) fn reject(&self, reason: AdmissionRejection) {
        let reason = match reason {
            AdmissionRejection::QueueUnavailable => "queue_unavailable",
            AdmissionRejection::BreakerOpen => "breaker_open",
        };
        self.in_scope(|| {
            tracing::warn!(event = "dispatch_rejected", tenant = %self.tenant,
            app = %self.module.app, version = %self.module.version, dispatch_id = self.dispatch_id,
            dispatch_token = self.token.0, reason, "dispatch admission refused")
        });
    }

    pub(super) fn in_scope<T>(&self, f: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.subscriber, || self.span.in_scope(f))
    }

    pub(super) fn acquired(&self, instance: InstanceId, source: &'static str) {
        let mut state = self.state.lock();
        state.instance = Some(instance);
        state.acquired = Some(source);
        self.span
            .record("instance", tracing::field::display(instance));
    }

    pub(super) fn begin_slice(&self) {
        let mut state = self.state.lock();
        state.usage.invoking = true;
        state.usage.slices = state.usage.slices.saturating_add(1);
    }

    pub(super) fn add_slice(&self, outcome: &DispatchOutcome) {
        let mut state = self.state.lock();
        let usage = &mut state.usage;
        usage.invoking = false;
        usage.cpu_ns = usage.cpu_ns.saturating_add(outcome.cpu_ns);
        usage.hot_wall_ns = usage.hot_wall_ns.saturating_add(outcome.wall_ns);
        usage.incomplete |= outcome.cpu_measurement == wisp_engine::CpuMeasurement::Incomplete;
        usage.overage_ns = outcome.overage_ns;
        usage.limit = outcome.execution_limit;
        usage.kill_phase = outcome.kill;
        usage.trap_kind = outcome.trap.as_ref().map(|trap| trap.kind);
        if outcome
            .trap
            .as_ref()
            .is_some_and(|trap| trap.kind == wisp_engine::TrapKind::CpuClockUnavailable)
        {
            state.outcome = Some((TerminalReason::CpuClockUnavailable, 500));
        }
    }

    pub(super) fn next_budget(&self) -> CpuBudget {
        self.budget.after(self.usage().cpu_ns)
    }

    pub(super) fn usage(&self) -> Usage {
        self.state.lock().usage
    }

    pub(super) fn select(&self, reason: TerminalReason, status: u16) {
        let mut state = self.state.lock();
        if !state.emitted {
            state.outcome = Some((reason, status));
        }
    }

    pub(super) fn result(
        &self,
        instance: InstanceId,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        completion: Completion,
        trap: Option<String>,
    ) -> DispatchResult {
        let state = self.state.lock();
        DispatchResult {
            dispatch_id: self.dispatch_id,
            status,
            headers,
            body,
            completion,
            cpu_ns: state.usage.cpu_ns,
            wall_ns: state.usage.hot_wall_ns,
            slices: state.usage.slices,
            instance,
            acquired: state.acquired.unwrap_or("none"),
            trap,
        }
    }

    pub(super) fn answer(
        &self,
        sender: &std::sync::mpsc::SyncSender<DispatchResult>,
        result: DispatchResult,
    ) {
        let mut state = self.state.lock();
        if state.responded {
            return;
        }
        state.responded = true;
        drop(state);
        answer(sender, result);
    }

    pub(super) fn host_trap(&self, code: TrapCode) {
        self.state.lock().guest_trap_code = Some(code);
    }

    pub(super) fn acquisition_failed(&self, error: &anyhow::Error) -> u16 {
        let mut state = self.state.lock();
        let exhausted = error
            .downcast_ref::<wisp_hibernate::snapshot::SnapshotError>()
            .is_some_and(wisp_hibernate::snapshot::SnapshotError::is_capacity_exhausted);
        let status = if exhausted { 503 } else { 500 };
        state.outcome = Some((
            if exhausted {
                TerminalReason::SnapshotCapacityUnavailable
            } else {
                TerminalReason::AcquisitionFailed
            },
            status,
        ));
        if let Some(error) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        {
            state.io_kind = Some(error.kind());
            state.os_error = error.raw_os_error();
        }
        status
    }

    pub(super) fn failure_result(&self, status: u16, code: &'static str) -> DispatchResult {
        // The legacy result uses zero before acquisition; the canonical event keeps it absent.
        self.result(
            self.instance().unwrap_or(InstanceId(0)),
            status,
            vec![],
            code.as_bytes().to_vec(),
            Completion::Trapped,
            Some(code.into()),
        )
    }

    pub(super) fn select_result(&self, result: &DispatchResult) {
        let mut state = self.state.lock();
        if state.emitted || state.outcome.is_some() {
            return;
        }
        let reason = match result.completion {
            Completion::Done => TerminalReason::Success,
            Completion::Trapped => TerminalReason::GuestTrap,
            Completion::BudgetKill | Completion::CleanKill => match state.usage.limit {
                Some(wisp_engine::ExecutionLimit::Wall) => TerminalReason::WallLimit,
                Some(wisp_engine::ExecutionLimit::Cpu) | None => TerminalReason::CpuLimit,
            },
            Completion::Parked => return,
        };
        state.outcome = Some((reason, result.status));
    }

    pub(super) fn dispatch_id(&self) -> u64 {
        self.dispatch_id
    }

    pub(super) fn instance(&self) -> Option<InstanceId> {
        self.state.lock().instance
    }

    pub(super) fn total_wall_ns(&self) -> Option<u64> {
        self.started_ns
            .zip((self.clock)())
            .and_then(|(start, end)| end.checked_sub(start))
    }

    pub(super) fn finish(&self) {
        let mut state = self.state.lock();
        if state.emitted {
            return;
        }
        let Some((reason, status)) = state.outcome else {
            return;
        };
        state.emitted = true;
        let usage = state.usage;
        let instance = state.instance.map(|id| id.0);
        let acquired = state.acquired;
        let io_kind = state.io_kind.map(|kind| format!("{kind:?}"));
        let os_error = state.os_error;
        let guest_trap_code = state.guest_trap_code.map(|code| format!("{code:?}"));
        let (error_kind, error_classification_complete) = match reason {
            TerminalReason::Success => (None, true),
            TerminalReason::GuestTrap | TerminalReason::CaptureFailed => match usage.trap_kind {
                Some(kind) => (
                    Some(kind.tenant_code()),
                    kind != wisp_engine::TrapKind::Other,
                ),
                None if state.guest_trap_code.is_some() => (Some("host_import_trap"), true),
                None => (Some("unclassified_guest_trap"), false),
            },
            TerminalReason::AcquisitionFailed | TerminalReason::RestoreFailed => {
                if state.io_kind.is_some() {
                    (Some("host_io"), true)
                } else {
                    (Some("unclassified_host_error"), false)
                }
            }
            TerminalReason::CpuLimit
            | TerminalReason::SnapshotCapacityUnavailable
            | TerminalReason::WallLimit
            | TerminalReason::RequestCancelled
            | TerminalReason::TransportFailure
            | TerminalReason::EntryRefused
            | TerminalReason::HostPanic
            | TerminalReason::CpuClockUnavailable
            | TerminalReason::CellStopped => (Some(reason.label()), true),
        };
        drop(state);
        let total_wall_ns = self.total_wall_ns();
        self.in_scope(|| tracing::info!(
            event = "dispatch_completed", tenant = %self.tenant,
            app = %self.module.app, version = %self.module.version,
            dispatch_token = self.token.0, dispatch_id = self.dispatch_id, dispatch_kind = self.kind,
            instance, acquired, outcome = reason.label(), status,
            error_kind, error_classification_complete, io_kind, os_error, guest_trap_code,
            cpu_scope = "dispatch_invocations", activation_cpu_included = false,
            cpu_ns = usage.cpu_ns, cpu_complete = !usage.incomplete && !usage.invoking,
            hot_wall_ns = usage.hot_wall_ns,
            hot_wall_complete = !usage.invoking,
            capture_hint_cpu_ns = usage.capture_hint_cpu_ns,
            capture_hint_wall_ns = usage.capture_hint_wall_ns,
            capture_hint_complete = !usage.hint_incomplete && !usage.hint_running,
            capture_hints = usage.capture_hints,
            total_guest_cpu_ns = usage.cpu_ns.saturating_add(usage.capture_hint_cpu_ns),
            total_guest_cpu_complete = !usage.incomplete && !usage.invoking && !usage.hint_incomplete && !usage.hint_running,
            total_wall_ns, total_wall_complete = total_wall_ns.is_some(),
            slices = usage.slices, cpu_budget_ns = self.budget.budget_ns(),
            execution_wall_cap_ns = u64::from(self.budget.wall_ms_cap) * 1_000_000,
            overage_ns = usage.overage_ns, kill_phase = usage.kill_phase.map(|phase| phase.as_str()),
            "dispatch completed"
        ));
    }

    pub(super) fn settlement(self: &Arc<Self>) -> CompletionSettlement {
        CompletionSettlement(self.clone())
    }
}

impl crate::hibernate_policy::CaptureObserver for DispatchCompletion {
    fn begin_hint(&self) {
        let mut state = self.state.lock();
        state.usage.hint_running = true;
        state.usage.capture_hints = state.usage.capture_hints.saturating_add(1);
    }

    fn end_hint(&self, outcome: &DispatchOutcome) {
        let mut state = self.state.lock();
        let usage = &mut state.usage;
        usage.hint_running = false;
        usage.capture_hint_cpu_ns = usage.capture_hint_cpu_ns.saturating_add(outcome.cpu_ns);
        usage.capture_hint_wall_ns = usage.capture_hint_wall_ns.saturating_add(outcome.wall_ns);
        usage.hint_incomplete |= outcome.cpu_measurement == wisp_engine::CpuMeasurement::Incomplete;
        if outcome.trap.is_some() {
            usage.trap_kind = outcome.trap.as_ref().map(|trap| trap.kind);
        }
    }
}

pub(super) struct CompletionSettlement(Arc<DispatchCompletion>);

impl Drop for CompletionSettlement {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.select(TerminalReason::HostPanic, 500);
        }
        self.0.finish();
    }
}

impl Cell {
    pub(super) fn completion_owner(
        &self,
        module: &ModuleId,
        record: &DispatchRecord,
    ) -> crate::Result<Arc<DispatchCompletion>> {
        let entry = self
            .modules
            .read()
            .get(module)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("module `{module}` is not registered"))?;
        anyhow::ensure!(
            !entry.tenant.trim().is_empty(),
            "module `{module}` has no tenant identity"
        );
        anyhow::ensure!(
            !module.app.trim().is_empty() && !module.version.trim().is_empty(),
            "module `{module}` has incomplete app/version identity"
        );
        DispatchCompletion::new(
            entry,
            record,
            CpuBudget {
                cpu_ms: record.cpu_budget_ms,
                wall_ms_cap: self.cfg.wall_ms_cap,
                spent_ns: 0,
            },
            self.completion_clock.read().clone(),
        )
    }
}

pub(super) struct HotInterval<'a>(&'a AtomicUsize);

impl<'a> HotInterval<'a> {
    pub(super) fn new(hot: &'a AtomicUsize) -> Self {
        hot.fetch_add(1, Ordering::Relaxed);
        Self(hot)
    }
}

impl Drop for HotInterval<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(super) mod test_events {
    use super::*;
    use serde_json::Value;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::{Layer, Registry};

    #[test]
    fn exhausted_dispatch_tokens_cannot_alias_an_earlier_owner() {
        let counter = AtomicU64::new(u64::MAX - 1);
        let last = DispatchToken::next(&counter).expect("last available token");
        assert_eq!(
            last.0,
            u64::MAX - 1,
            "the final allocatable token must retain its value"
        );
        assert!(
            DispatchToken::next(&counter).is_err(),
            "token allocation must refuse wrapping into an earlier owner"
        );
        assert_eq!(
            counter.load(Ordering::Relaxed),
            u64::MAX,
            "refusal cannot reset the identity sequence"
        );
    }

    #[derive(Clone, Default)]
    pub(crate) struct Events(Arc<Mutex<Vec<serde_json::Map<String, Value>>>>);

    #[derive(Default)]
    struct Fields(serde_json::Map<String, Value>);

    impl Visit for Fields {
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().into(), value.into());
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0.insert(field.name().into(), value.into());
        }
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.0.insert(field.name().into(), value.into());
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().into(), value.into());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().into(), format!("{value:?}").into());
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for Events {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            if fields.0.get("event").and_then(Value::as_str) == Some("dispatch_completed") {
                self.0.lock().push(fields.0);
            }
        }
    }

    impl Events {
        pub(crate) fn install(&self) -> tracing::dispatcher::DefaultGuard {
            tracing::dispatcher::set_default(&tracing::Dispatch::new(
                Registry::default().with(self.clone()),
            ))
        }

        pub(crate) fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
            let subscriber = tracing::Dispatch::new(Registry::default().with(self.clone()));
            tracing::dispatcher::with_default(&subscriber, f)
        }

        pub(crate) fn records(&self) -> Vec<serde_json::Map<String, Value>> {
            self.0.lock().clone()
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
