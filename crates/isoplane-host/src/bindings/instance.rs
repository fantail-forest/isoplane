//! The per-instance half of the vtable's state, shared between the slot and
//! the bindings moved into the engine at activation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::status::TrapCode;
use parking_lot::Mutex;

use super::CellRuntime;
use crate::meters::Meters;
use crate::response::ResponseTable;
use crate::{InstanceId, ModuleId};

/// A slot-local request generation, independent of caller-supplied dispatch IDs.
#[derive(Clone, Copy)]
pub(crate) struct DispatchGeneration(u64);

/// Established ownership and numeric budgets for an instance.
#[derive(Debug, Clone)]
pub struct Tenancy {
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub lifecycle: HandlerLifecycle,
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
    pub lifecycle: HandlerLifecycle,
    pub allowlist: Option<Vec<String>>,
    /// Exact peers an operator lets this application reach where the node's policy refuses.
    pub endpoint_exceptions: Arc<[std::net::SocketAddr]>,
    pub meters: Arc<Meters>,
    /// `None` in tests that exercise the slot machinery without a cell.
    pub rt: Option<Arc<CellRuntime>>,

    /// The frozen slice clock, written once per slice by the cell.
    pub(super) clock_ms: AtomicU64,
    /// The current top-level work item, the unit the future cap applies to.
    pub(super) dispatch: AtomicU64,
    pub(super) subrequests: AtomicU32,
    /// Bounds log volume independently of the CPU it already costs.
    pub(super) log_bytes: AtomicU32,
    pub(super) entropy_calls: AtomicU64,
    /// A trap the vtable owes the guest but cannot raise from a call that
    /// returns an `i64`; the cell converts it after the call returns.
    pub(super) trap_latch: Mutex<Option<TrapCode>>,
    /// Set when the dispatch enters its grace window: every import it calls
    /// then returns `Cancelled` at once, so the unwind has something to unwind
    /// for and no new work starts on budget the dispatch does not have.
    pub(super) cancelled: AtomicBool,
    dispatch_generation: AtomicU64,
    cancelled_generation: AtomicU64,
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
            lifecycle: tenancy.lifecycle,
            allowlist: tenancy.allowlist,
            endpoint_exceptions: Arc::new([]),
            meters,
            rt,
            clock_ms: AtomicU64::new(0),
            dispatch: AtomicU64::new(0),
            subrequests: AtomicU32::new(0),
            log_bytes: AtomicU32::new(0),
            entropy_calls: AtomicU64::new(0),
            trap_latch: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            dispatch_generation: AtomicU64::new(0),
            cancelled_generation: AtomicU64::new(0),
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
    pub fn for_test(instance: InstanceId, lifecycle: HandlerLifecycle, limits: Limits) -> Self {
        Self::new(
            instance,
            Tenancy {
                module: ModuleId::new("test", "v1"),
                tenant: "t".into(),
                limits,
                lifecycle,
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

    /// Resets the per-dispatch counters, including the poison, which under
    /// `reuse` may be left from a clean kill a moment ago.
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
        self.cancelled.store(false, Ordering::Relaxed);
        self.cancel_future.store(0, Ordering::Relaxed);
        self.begin_slice(now_ms, dispatch);
    }

    pub fn set_cancel_future(&self, id: u64) {
        self.cancel_future.store(id, Ordering::Relaxed);
    }

    pub fn cancel_future(&self) -> u64 {
        self.cancel_future.load(Ordering::Relaxed)
    }

    pub fn is_cancelled(&self) -> bool {
        let generation = self.dispatch_generation.load(Ordering::Acquire);
        self.cancelled.load(Ordering::Relaxed)
            || generation != 0 && self.cancelled_generation.load(Ordering::Acquire) >= generation
    }

    pub(crate) fn dispatch_generation(&self) -> DispatchGeneration {
        DispatchGeneration(self.dispatch_generation.load(Ordering::Acquire))
    }

    pub(crate) fn cancel_generation(&self, generation: DispatchGeneration) {
        self.cancelled_generation
            .fetch_max(generation.0, Ordering::AcqRel);
    }

    /// Returns false if already poisoned, so the cancellation future is
    /// completed exactly once.
    pub fn poison_imports(&self) -> bool {
        !self.cancelled.swap(true, Ordering::Relaxed)
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
    /// Without consuming it: the cell decides whether a call ends in a
    /// tombstone while it still holds the guard, and `take_trap` comes later.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delayed_transport_cancellation_cannot_poison_a_reused_instance() {
        let ctx = InstanceCtx::for_test(
            InstanceId(1),
            HandlerLifecycle::Reuse,
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 31457280,
                instance_memory_max: 134217728,
                cpu_ms_per_http_dispatch: 100,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
        );
        ctx.begin_dispatch(0, 10);
        let first = ctx.dispatch_generation();
        ctx.cancel_generation(first);
        assert!(
            ctx.is_cancelled(),
            "disconnect must poison the active request immediately"
        );
        ctx.begin_dispatch(0, 2);
        let second = ctx.dispatch_generation();
        ctx.cancel_generation(first);
        assert!(
            !ctx.is_cancelled(),
            "an old body's drop must not cancel a later request"
        );
        ctx.cancel_generation(second);
        ctx.cancel_generation(first);
        assert!(
            ctx.is_cancelled(),
            "a delayed older cancellation must not undo current cancellation"
        );
    }
}
