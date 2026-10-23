//! The per-instance half of the vtable's state, shared between the slot and
//! the bindings moved into the engine at activation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_abi::config::Limits;
use isoplane_abi::status::TrapCode;
use parking_lot::Mutex;

use super::dispatch::DispatchState;
use super::CellRuntime;
use crate::meters::Meters;
use crate::response::ResponseTable;
use crate::{InstanceId, ModuleId};

/// How many dispatches an instance has begun: a later dispatch has a greater
/// generation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DispatchGeneration(pub(super) u64);

/// Established ownership and numeric budgets for an instance.
#[derive(Debug, Clone)]
pub struct Tenancy {
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub allowlist: Option<Vec<String>>,
}

/// Host-side budgets for each guest instance. Zero denies the corresponding resource.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuestResourceLimits {
    /// Simultaneously retained native socket, timer and stream handles.
    pub native_handles: usize,
    /// Maximum incoming guest log payload bytes accepted per dispatch.
    pub log_bytes_per_dispatch: u32,
    /// Maximum guest trace spans retained in one instance.
    pub trace_spans: usize,
    /// Maximum UTF-8 name bytes retained by the instance's guest trace spans.
    pub trace_name_bytes: usize,
    /// Maximum responses retained in one instance.
    pub response_handles: usize,
}

impl Default for GuestResourceLimits {
    fn default() -> Self {
        Self {
            native_handles: 1024,
            log_bytes_per_dispatch: 256 * 1024,
            trace_spans: 1024,
            trace_name_bytes: 256 * 1024,
            response_handles: 1024,
        }
    }
}

/// The per-instance half of the vtable's state, shared between the slot and
/// the [`CellBindings`] moved into the engine at activation, so the cell can
/// freeze the clock, read the trap latch and collect the response for an
/// instance whose bindings it no longer owns.
pub struct InstanceCtx {
    pub(crate) background: Arc<crate::background::Background>,
    pub(crate) admission: Mutex<Option<crate::admission::InstanceReservation>>,
    pub(crate) snapshot: Mutex<Option<Box<dyn isoplane_hibernate::snapshot::SnapshotReservation>>>,
    pub instance: InstanceId,
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub(super) guest_resources: GuestResourceLimits,
    pub allowlist: Option<Vec<String>>,
    /// Exact peers an operator lets this application reach where the node's policy refuses.
    pub endpoint_exceptions: Arc<[std::net::SocketAddr]>,
    pub meters: Arc<Meters>,
    /// `None` in tests that exercise the slot machinery without a cell.
    pub rt: Option<Arc<CellRuntime>>,

    /// The frozen slice clock, written once per slice by the cell.
    pub(super) clock_ms: AtomicU64,
    /// The dispatches in progress on this instance.
    dispatches: Mutex<HashMap<u64, Arc<DispatchState>>>,
    /// The dispatch the current or most recent slice runs; the imports a
    /// slice calls act on it.
    current: Mutex<Arc<DispatchState>>,
    /// Current outside any dispatch: during initialization and host-initiated
    /// slices such as the pre-capture GC hint.
    no_dispatch: Arc<DispatchState>,
    pub(super) entropy_calls: AtomicU64,
    /// A trap the vtable owes the guest but cannot raise from an import call
    /// that returns an `i64`; the cell converts it after the slice returns.
    pub(super) trap_latch: Mutex<Option<TrapCode>>,
    dispatch_generation: AtomicU64,
    /// Host-to-guest bytes for `entropy_fill`, copied in by the engine after
    /// the call.
    pub(super) out_buffer: Mutex<Vec<u8>>,
    pub responses: Mutex<ResponseTable>,
    pub(super) http: Arc<super::http_scope::HttpScope>,
    pub(super) spans: Mutex<super::observability::TraceSpans>,
    pub(super) native_io: Mutex<super::native_io::Resources>,
}

impl InstanceCtx {
    pub(crate) fn report_background(&self) {
        let connections = self.rt.as_ref().map_or(0, |rt| {
            rt.services.egress.instance_connections(self.instance)
        });
        self.background
            .report(&self.tenant, &self.module, connections);
    }

    pub(crate) fn configure_resources(&mut self, limits: GuestResourceLimits) {
        self.guest_resources = limits;
        self.responses = Mutex::new(ResponseTable::new(
            limits.response_handles,
            self.limits.instance_memory_max,
        ));
        self.native_io = Mutex::new(super::native_io::Resources::new(
            super::native_io::ResourceLimits {
                handles: super::native_io::ResourceCount(limits.native_handles),
            },
        ));
    }

    /// Uses the slice start for native timer readiness and latches cancellation
    /// before guest entry. Socket and DNS readiness can change asynchronously.
    pub(crate) fn begin_guest_slice(&self, now: tokio::time::Instant) {
        self.native_io.lock().begin_slice(now);
        self.current().cancellation().begin_slice();
    }

    /// The current dispatch's cancellation future, exactly once per requested
    /// cancellation, after the slice it was requested in has ended.
    pub(crate) fn take_undelivered_cancellation(&self) -> Option<u64> {
        self.current().take_undelivered_cancellation()
    }

    /// When the guest's current slice began, if one has.
    pub(crate) fn slice_start(&self) -> Option<tokio::time::Instant> {
        self.native_io.lock().slice_start()
    }

    /// Timers the instance holds as native resources.
    pub(crate) fn native_timers(&self) -> usize {
        self.native_io.lock().timers()
    }

    /// The timers an idle instance is woken for, when its template opted in.
    pub(crate) fn idle_timers(&self) -> Option<super::native_io::IdleTimers> {
        self.native_io.lock().idle_timers()
    }

    pub(crate) fn release_native_resources(&self) {
        self.http.close();
        self.native_io.lock().clear();
        self.spans.lock().clear();
        self.responses.lock().clear();
        for dispatch in self.dispatches.lock().values() {
            dispatch.close();
        }
    }

    /// Reclaims every host future the instance held, including work it kept
    /// pending while idle.
    pub(crate) fn release_futures(&self) {
        if let Some(rt) = &self.rt {
            rt.futures.release_instance(self.instance);
        }
    }

    pub(crate) fn has_outbound_http(&self) -> bool {
        self.http.is_active()
    }

    /// Whether any dispatch in progress is still reading its request body.
    pub(crate) fn has_active_request_body(&self) -> bool {
        self.dispatches.lock().values().any(|dispatch| {
            dispatch
                .request_body
                .lock()
                .as_ref()
                .is_some_and(|body| body.is_active())
        })
    }

    /// Whether any dispatch in progress has started streaming its response.
    pub(crate) fn has_open_transport(&self) -> bool {
        self.dispatches.lock().values().any(|dispatch| {
            dispatch
                .transport
                .lock()
                .as_ref()
                .is_some_and(|stream| stream.has_started())
        })
    }

    pub fn new(
        instance: InstanceId,
        tenancy: Tenancy,
        meters: Arc<Meters>,
        rt: Option<Arc<CellRuntime>>,
    ) -> Self {
        let no_dispatch = DispatchState::new(0);
        let background = Arc::new(crate::background::Background::new(instance, rt.as_deref()));
        Self {
            background,
            admission: Mutex::new(None),
            snapshot: Mutex::new(None),
            instance,
            module: tenancy.module,
            tenant: tenancy.tenant,
            limits: tenancy.limits,
            guest_resources: GuestResourceLimits::default(),
            allowlist: tenancy.allowlist,
            endpoint_exceptions: Arc::new([]),
            meters,
            rt,
            clock_ms: AtomicU64::new(0),
            dispatches: Mutex::new(HashMap::new()),
            current: Mutex::new(no_dispatch.clone()),
            no_dispatch,
            entropy_calls: AtomicU64::new(0),
            trap_latch: Mutex::new(None),
            dispatch_generation: AtomicU64::new(0),
            out_buffer: Mutex::new(Vec::new()),
            responses: Mutex::new(ResponseTable::new(
                GuestResourceLimits::default().response_handles,
                tenancy.limits.instance_memory_max,
            )),
            http: super::http_scope::HttpScope::new(),
            spans: Mutex::new(super::observability::TraceSpans::default()),
            native_io: Mutex::new(super::native_io::Resources::new(
                super::native_io::ResourceLimits::default(),
            )),
        }
    }

    /// A context with no cell behind it, for tests of the slot machinery.
    pub fn for_test(instance: InstanceId, limits: Limits) -> Self {
        Self::new(
            instance,
            Tenancy {
                module: ModuleId::new("test", "v1"),
                tenant: "t".into(),
                limits,
                allowlist: None,
            },
            Arc::new(Meters::new()),
            None,
        )
    }

    /// Freezes the coarse clock for one slice of `dispatch`, which the slice's
    /// imports then act on; a slice of a dispatch the instance has not seen
    /// begins it, and dispatch 0 is outside any dispatch. Monotonic per
    /// instance, so a host clock stepping backwards is not visible inside the
    /// sandbox.
    pub fn begin_slice(&self, now_ms: u64, dispatch: u64) {
        let prev = self.clock_ms.load(Ordering::Relaxed);
        self.clock_ms.store(now_ms.max(prev), Ordering::Relaxed);
        let state = match dispatch {
            0 => self.no_dispatch.clone(),
            _ => self.dispatch_state(dispatch),
        };
        *self.current.lock() = state;
    }

    fn dispatch_state(&self, dispatch: u64) -> Arc<DispatchState> {
        let mut dispatches = self.dispatches.lock();
        if let Some(state) = dispatches.get(&dispatch) {
            return state.clone();
        }
        let state = DispatchState::new(dispatch);
        dispatches.insert(dispatch, state.clone());
        self.dispatch_generation.fetch_add(1, Ordering::AcqRel);
        state
    }

    /// Starts a new dispatch and makes it current.
    pub fn begin_dispatch(&self, now_ms: u64, dispatch: u64) {
        self.start_dispatch(now_ms, dispatch);
    }

    /// Starts the state for a new dispatch and makes it current. A dispatch
    /// with the same id that is still in progress is ended first.
    pub(crate) fn start_dispatch(&self, now_ms: u64, dispatch: u64) -> Arc<DispatchState> {
        self.end_dispatch(dispatch);
        self.begin_slice(now_ms, dispatch);
        self.current()
    }

    /// Ends a dispatch's state, closing what it held open.
    pub(crate) fn end_dispatch(&self, dispatch: u64) {
        let mut dropped_spans = 0;
        if let Some(state) = self.dispatches.lock().remove(&dispatch) {
            dropped_spans = state.trace_dropped.load(Ordering::Relaxed);
            let background: Arc<dyn crate::reactor_accounting::Observer> = self.background.clone();
            state.reactor_attribution.set(Some(&background));
            state.close();
        }
        let abandoned_spans = self.spans.lock().end_dispatch(dispatch);
        if abandoned_spans != 0 || dropped_spans != 0 {
            tracing::debug!(tenant = %self.tenant, instance = %self.instance, dispatch,
                abandoned_spans, dropped_spans, "guest trace resources released");
        }
        self.responses.lock().end_dispatch(dispatch);
        let mut current = self.current.lock();
        if current.id() == dispatch {
            *current = self.no_dispatch.clone();
        }
    }

    /// A dispatch in progress on this instance.
    pub(crate) fn dispatch(&self, dispatch: u64) -> Option<Arc<DispatchState>> {
        self.dispatches.lock().get(&dispatch).cloned()
    }

    /// The dispatch the current slice runs.
    pub(crate) fn current(&self) -> Arc<DispatchState> {
        self.current.lock().clone()
    }

    pub(crate) fn reactor_usage(&self) -> crate::reactor_accounting::TaskUsage {
        crate::reactor_accounting::TaskUsage::new(
            self.meters.clone(),
            self.current().reactor_attribution.clone(),
        )
    }

    pub(crate) fn spawn(
        &self,
        reactor: &tokio::runtime::Handle,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        reactor.spawn(self.reactor_usage().measure(future))
    }

    /// Whether the current dispatch is cancelled as the guest sees it: as it
    /// stood when the guest's current slice began.
    pub fn is_cancelled(&self) -> bool {
        self.current().cancellation().observed()
    }

    pub(crate) fn dispatch_generation(&self) -> DispatchGeneration {
        DispatchGeneration(self.dispatch_generation.load(Ordering::Acquire))
    }

    pub fn slice_clock_ms(&self) -> u64 {
        self.clock_ms.load(Ordering::Relaxed)
    }
    /// The dispatch the current slice runs; 0 outside any dispatch.
    pub fn dispatch_id(&self) -> u64 {
        self.current().id()
    }
    pub fn entropy_calls(&self) -> u64 {
        self.entropy_calls.load(Ordering::Relaxed)
    }
    pub fn take_trap(&self) -> Option<TrapCode> {
        self.trap_latch.lock().take()
    }
    /// Without consuming it: the cell decides whether a call ends the
    /// instance while it still holds the guard, and `take_trap` comes later.
    pub fn has_trap(&self) -> bool {
        self.trap_latch.lock().is_some()
    }
    pub fn latch_trap(&self, code: TrapCode) {
        let mut g = self.trap_latch.lock();
        if g.is_none() {
            *g = Some(code);
        }
    }
    pub fn take_out_buffer(&self) -> Vec<u8> {
        std::mem::take(&mut self.out_buffer.lock())
    }
}

impl Drop for InstanceCtx {
    fn drop(&mut self) {
        self.http.close();
    }
}
