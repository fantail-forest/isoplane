//! The per-instance half of the vtable's state, shared between the slot and
//! the bindings moved into the engine at activation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_abi::config::Limits;
use isoplane_abi::status::TrapCode;
use parking_lot::Mutex;

use super::CellRuntime;
use crate::meters::Meters;
use crate::response::ResponseTable;
use crate::{InstanceId, ModuleId};

/// A slot-local request generation, independent of caller-supplied dispatch IDs.
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
}

impl Default for GuestResourceLimits {
    fn default() -> Self {
        Self {
            native_handles: 1024,
            log_bytes_per_dispatch: 256 * 1024,
        }
    }
}

/// The per-instance half of the vtable's state, shared between the slot and
/// the [`CellBindings`] moved into the engine at activation, so the cell can
/// freeze the clock, read the trap latch and collect the response for an
/// instance whose bindings it no longer owns.
pub struct InstanceCtx {
    pub(crate) snapshot: Mutex<Option<Box<dyn isoplane_hibernate::snapshot::SnapshotReservation>>>,
    pub instance: InstanceId,
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub(super) guest_resources: GuestResourceLimits,
    pub allowlist: Option<Vec<String>>,
    /// Exact peers an operator lets this application reach where the node's policy refuses.
    pub endpoint_exceptions: Arc<[std::net::SocketAddr]>,
    /// Whether the guest may show notices meant only for local development.
    pub development_notices: bool,
    pub meters: Arc<Meters>,
    pub(crate) reactor_observer:
        Mutex<Option<std::sync::Weak<dyn crate::reactor_accounting::Observer>>>,
    /// `None` in tests that exercise the slot machinery without a cell.
    pub rt: Option<Arc<CellRuntime>>,

    /// The frozen slice clock, written once per slice by the cell.
    pub(super) clock_ms: AtomicU64,
    /// The current top-level work item, the unit the future cap applies to.
    pub(super) dispatch: AtomicU64,
    pub(super) subrequests: AtomicU32,
    /// Bounds log volume independently of its occupied-worker cost.
    pub(super) log_bytes: AtomicU32,
    pub(super) entropy_calls: AtomicU64,
    /// A trap the vtable owes the guest but cannot raise from a call that
    /// returns an `i64`; the cell converts it after the call returns.
    pub(super) trap_latch: Mutex<Option<TrapCode>>,
    /// Requested when the dispatch enters its grace window or the host ends
    /// it; from the guest's next slice every import it calls returns
    /// `Cancelled` at once, so the unwind has something to unwind for and no
    /// new work starts on budget the dispatch does not have.
    cancellation: super::cancellation::Cancellation,
    dispatch_generation: AtomicU64,
    /// The dispatch's implicit cancellation future; the host cancels by
    /// completing it, never by entering the instance.
    pub(super) cancel_future: AtomicU64,
    /// Host-to-guest bytes for `entropy_fill`, copied in by the engine after
    /// the call.
    pub(super) out_buffer: Mutex<Vec<u8>>,
    pub responses: Mutex<ResponseTable>,
    pub(super) spans: Mutex<HashMap<i32, (String, u64)>>,
    pub(super) next_span: AtomicU32,
    pub(super) http: Mutex<Arc<super::http_scope::HttpScope>>,
    pub(crate) request_body: Mutex<Option<Arc<super::request_body::RequestBodyScope>>>,
    pub(super) native_io: Mutex<super::native_io::Resources>,
}

impl InstanceCtx {
    pub(crate) fn configure_resources(&mut self, limits: GuestResourceLimits) {
        self.guest_resources = limits;
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
        self.cancellation.begin_slice(self.dispatch_generation());
    }

    /// True exactly once per requested cancellation, after the slice it was
    /// requested in has ended.
    pub(crate) fn take_undelivered_cancellation(&self) -> bool {
        self.cancellation.take_undelivered()
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
        self.native_io.lock().clear();
        self.end_http_dispatch();
    }

    pub(crate) fn end_http_dispatch(&self) {
        self.http.lock().close();
        if let Some(body) = self.request_body.lock().take() {
            body.close();
        }
    }

    pub(crate) fn has_outbound_http(&self) -> bool {
        self.http.lock().is_active()
    }

    pub fn new(
        instance: InstanceId,
        tenancy: Tenancy,
        meters: Arc<Meters>,
        rt: Option<Arc<CellRuntime>>,
    ) -> Self {
        Self {
            snapshot: Mutex::new(None),
            instance,
            module: tenancy.module,
            tenant: tenancy.tenant,
            limits: tenancy.limits,
            guest_resources: GuestResourceLimits::default(),
            allowlist: tenancy.allowlist,
            endpoint_exceptions: Arc::new([]),
            development_notices: false,
            meters,
            rt,
            clock_ms: AtomicU64::new(0),
            dispatch: AtomicU64::new(0),
            subrequests: AtomicU32::new(0),
            log_bytes: AtomicU32::new(0),
            entropy_calls: AtomicU64::new(0),
            trap_latch: Mutex::new(None),
            cancellation: Default::default(),
            reactor_observer: Mutex::new(None),
            dispatch_generation: AtomicU64::new(0),
            cancel_future: AtomicU64::new(0),
            out_buffer: Mutex::new(Vec::new()),
            responses: Mutex::new(ResponseTable::default()),
            spans: Mutex::new(HashMap::new()),
            next_span: AtomicU32::new(1),
            http: Mutex::new(super::http_scope::HttpScope::new()),
            request_body: Mutex::new(None),
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

    /// Freezes the coarse clock for one slice. Monotonic per instance, so a
    /// host clock stepping backwards is not visible inside the sandbox.
    pub fn begin_slice(&self, now_ms: u64, dispatch: u64) {
        let prev = self.clock_ms.load(Ordering::Relaxed);
        self.clock_ms.store(now_ms.max(prev), Ordering::Relaxed);
        self.dispatch.store(dispatch, Ordering::Relaxed);
    }

    /// Resets the per-dispatch counters, including the poison, which a clean
    /// kill a moment ago may have left.
    pub fn begin_dispatch(&self, now_ms: u64, dispatch: u64) {
        if let Some(body) = self.request_body.lock().take() {
            body.close();
        }
        let mut http = self.http.lock();
        http.close();
        *http = super::http_scope::HttpScope::new();
        drop(http);
        self.subrequests.store(0, Ordering::Relaxed);
        self.log_bytes.store(0, Ordering::Relaxed);
        self.dispatch_generation.fetch_add(1, Ordering::AcqRel);
        self.cancellation.begin_dispatch();
        self.cancel_future.store(0, Ordering::Relaxed);
        self.begin_slice(now_ms, dispatch);
    }

    pub(crate) fn reactor_usage(&self) -> crate::reactor_accounting::TaskUsage {
        let observer = self
            .reactor_observer
            .lock()
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        crate::reactor_accounting::TaskUsage::new(self.meters.clone(), observer)
    }

    pub(crate) fn spawn(
        &self,
        reactor: &tokio::runtime::Handle,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        reactor.spawn(self.reactor_usage().measure(future))
    }

    pub fn set_cancel_future(&self, id: u64) {
        self.cancel_future.store(id, Ordering::Relaxed);
    }

    pub fn cancel_future(&self) -> u64 {
        self.cancel_future.load(Ordering::Relaxed)
    }

    /// Whether the dispatch is cancelled as the guest sees it: as it stood
    /// when the guest's current slice began.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.observed()
    }

    pub(crate) fn dispatch_generation(&self) -> DispatchGeneration {
        DispatchGeneration(self.dispatch_generation.load(Ordering::Acquire))
    }

    /// Requests cancellation of the dispatch of `generation`, if it is still
    /// the current one when the guest's next slice begins.
    pub(crate) fn cancel_generation(&self, generation: DispatchGeneration) {
        self.cancellation.request_generation(generation);
    }

    /// Requests cancellation of the current dispatch; the guest sees it from
    /// its next slice. Returns false if it was already requested.
    pub fn request_cancellation(&self) -> bool {
        self.cancellation.request()
    }

    pub fn slice_clock_ms(&self) -> u64 {
        self.clock_ms.load(Ordering::Relaxed)
    }
    pub fn dispatch_id(&self) -> u64 {
        self.dispatch.load(Ordering::Relaxed)
    }
    pub fn subrequests(&self) -> u32 {
        self.subrequests.load(Ordering::Relaxed)
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
        self.http.get_mut().close();
        if let Some(body) = self.request_body.get_mut().take() {
            body.close();
        }
    }
}
