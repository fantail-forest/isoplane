//! Routes guest capabilities through host-owned services.
//! The coarse clock is fixed for each serving slice.
//! Scalar arguments follow declaration order after the buffer pair.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use rand::RngCore;

use wisp_abi::calls::*;
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::ResponseHead;
use wisp_abi::imports::{self, ImportKind};
use wisp_abi::result::*;
use wisp_abi::status::TrapCode;
use wisp_engine::{HostBindings, ImportCall};

use crate::egress::HostEgressPolicy;
use crate::futures::{FutureTable, WakeTarget, INVALID_FUTURE};
use crate::meters::{CpuInterval, Meters};
use crate::metrics::Metrics;
use crate::response::ResponseTable;
use crate::{InstanceId, ModuleId};

/// What each import's `scalars` entries mean; the contract crate's `BufSpec`
/// is the authority on where the buffer pair sits.
pub const SCALARS: &[(&str, &[&str])] = &[
    ("log_emit", &["level"]),
    ("trace_span_end", &["span"]),
    ("clock_deadline_in", &["ms"]),
    ("resp_write", &["response_handle"]),
    ("resp_finish", &["response_handle"]),
    ("http_body_read", &["body_handle", "max_len"]),
    ("init_unit_begin", &["unit"]),
    ("init_unit_deferred", &["unit"]),
    // Answered by the engine and never reaching this vtable, but declared.
    ("unsafe_to_trap", &["on"]),
];

/// How a completed host future gets back to an execution-pool thread.
pub trait WakeSink: Send + Sync + 'static {
    fn signal(&self, owner: InstanceId);
}

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync + 'static {
    /// `target.addr` is the pinned address from the egress check; an
    /// implementation that re-resolves `target.url` has thrown the rebinding
    /// control away.
    async fn fetch(
        &self,
        target: wisp_abi::host::PinnedTarget,
        req: FetchRequest,
    ) -> Result<FetchResponse, FutureError>;

    async fn read_body(&self, handle: u32, max_len: usize) -> Result<BodyChunk, FutureError>;
}

/// Everything the vtable routes to.
pub struct Services {
    pub egress: Arc<HostEgressPolicy>,
    pub fetcher: Arc<dyn Fetcher>,
}

/// The per-cell machinery an instance's bindings share; per-instance state
/// lives in [`InstanceCtx`].
pub struct CellRuntime {
    pub services: Arc<Services>,
    pub futures: Arc<FutureTable>,
    pub metrics: Arc<Metrics>,
    pub wake: Arc<dyn WakeSink>,
    /// Reactor threads own all I/O; an execution thread never awaits.
    pub reactor: tokio::runtime::Handle,
}

impl CellRuntime {
    /// The one completion path, for the vtable's own futures and for
    /// `Cell::complete_future`.
    pub fn settle_future(&self, id: u64, res: wisp_abi::result::ResultRecord) {
        let WakeTarget::Instance(owner) = self.futures.complete(id, res) else {
            return;
        };
        self.wake.signal(owner);
    }
}

/// Whose instance this is and under what terms.
#[derive(Debug, Clone)]
pub struct Tenancy {
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub lifecycle: HandlerLifecycle,
    pub allowlist: Option<Vec<String>>,
}

/// The per-instance half of the vtable's state, shared between the slot and
/// the [`CellBindings`] moved into the engine at activation, so the cell can
/// freeze the clock, read the trap latch and collect the response for an
/// instance whose bindings it no longer owns.
pub struct InstanceCtx {
    pub instance: InstanceId,
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub lifecycle: HandlerLifecycle,
    pub allowlist: Option<Vec<String>>,
    pub meters: Arc<Meters>,
    /// `None` in tests that exercise the slot machinery without a cell.
    pub rt: Option<Arc<CellRuntime>>,

    /// The frozen slice clock, written once per slice by the cell.
    clock_ms: AtomicU64,
    /// The current top-level work item, the unit the future cap applies to.
    dispatch: AtomicU64,
    subrequests: AtomicU32,
    /// Bounds log volume independently of the CPU it already costs.
    log_bytes: AtomicU32,
    entropy_calls: AtomicU64,
    /// A trap the vtable owes the guest but cannot raise from a call that
    /// returns an `i64`; the cell converts it after the call returns.
    trap_latch: Mutex<Option<TrapCode>>,
    /// Set when the dispatch enters its grace window: every import it calls
    /// then returns `Cancelled` at once, so the unwind has something to unwind
    /// for and no new work starts on budget the dispatch does not have.
    cancelled: AtomicBool,
    /// The dispatch's implicit cancellation future; the host cancels by
    /// completing it, never by entering the instance.
    cancel_future: AtomicU64,
    /// Host-to-guest bytes for `entropy_fill`, copied in by the engine after
    /// the call.
    out_buffer: Mutex<Vec<u8>>,
    pub responses: Mutex<ResponseTable>,
    spans: Mutex<HashMap<i32, (String, u64)>>,
    next_span: AtomicU32,
}

impl InstanceCtx {
    pub fn new(
        instance: InstanceId,
        tenancy: Tenancy,
        meters: Arc<Meters>,
        rt: Option<Arc<CellRuntime>>,
    ) -> Self {
        Self {
            instance,
            module: tenancy.module,
            tenant: tenancy.tenant,
            limits: tenancy.limits,
            tier: tenancy.tier,
            lifecycle: tenancy.lifecycle,
            allowlist: tenancy.allowlist,
            meters,
            rt,
            clock_ms: AtomicU64::new(0),
            dispatch: AtomicU64::new(0),
            subrequests: AtomicU32::new(0),
            log_bytes: AtomicU32::new(0),
            entropy_calls: AtomicU64::new(0),
            trap_latch: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            cancel_future: AtomicU64::new(0),
            out_buffer: Mutex::new(Vec::new()),
            responses: Mutex::new(ResponseTable::default()),
            spans: Mutex::new(HashMap::new()),
            next_span: AtomicU32::new(1),
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
        self.subrequests.store(0, Ordering::Relaxed);
        self.log_bytes.store(0, Ordering::Relaxed);
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
        self.cancelled.load(Ordering::Relaxed)
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

/// One per instance, moved into the engine at activation.
pub struct CellBindings {
    ctx: Arc<InstanceCtx>,
}

impl CellBindings {
    pub fn new(ctx: Arc<InstanceCtx>) -> Self {
        Self { ctx }
    }
    pub fn boxed(ctx: Arc<InstanceCtx>) -> Box<dyn HostBindings> {
        Box::new(Self::new(ctx))
    }
    pub fn ctx(&self) -> &Arc<InstanceCtx> {
        &self.ctx
    }
    pub fn take_out_buffer(&mut self) -> Vec<u8> {
        self.ctx.take_out_buffer()
    }

    fn rt(&self) -> Option<&Arc<CellRuntime>> {
        self.ctx.rt.as_ref()
    }

    /// A refusal latches [`TrapCode::TooManyFutures`], because `call_future`
    /// returns a bare `u64` and cannot trap directly.
    fn alloc(&self, import: &'static str) -> Option<u64> {
        let rt = self.rt()?;
        let dispatch = self.ctx.dispatch_id();
        match rt.futures.alloc(
            self.ctx.instance,
            dispatch,
            import,
            self.ctx.limits.pending_futures_per_dispatch,
        ) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(
                    tenant = %self.ctx.tenant, instance = %self.ctx.instance,
                    limit = e.limit, "pending-futures cap exceeded"
                );
                self.ctx.latch_trap(TrapCode::TooManyFutures);
                None
            }
        }
    }

    fn settle(rt: &Arc<CellRuntime>, id: u64, res: ResultRecord) {
        rt.settle_future(id, res);
    }

    /// For policy refusals: the guest still learns through the future.
    fn settle_now(&self, id: u64, res: ResultRecord) -> u64 {
        if let Some(rt) = self.rt() {
            Self::settle(rt, id, res);
        }
        id
    }

    fn decode<T: serde::de::DeserializeOwned>(&self, payload: &[u8]) -> Option<T> {
        match postcard::from_bytes(payload) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(tenant = %self.ctx.tenant, error = %e, "malformed import payload");
                self.ctx.latch_trap(TrapCode::Protocol);
                None
            }
        }
    }

    fn check_subrequest(&self) -> Result<(), FutureError> {
        let n = self.ctx.subrequests.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.ctx.limits.subrequests_per_dispatch {
            return Err(FutureError::Denied(format!(
                "subrequest cap of {} per dispatch exceeded",
                self.ctx.limits.subrequests_per_dispatch
            )));
        }
        Ok(())
    }
}

fn ok_bytes<T: serde::Serialize>(v: &T) -> ResultRecord {
    match ResultRecord::ok(v) {
        Ok(r) => r,
        Err(e) => ResultRecord::Err(FutureError::Io(format!("encode: {e}"))),
    }
}

fn from_typed<T: serde::Serialize>(r: Result<T, FutureError>) -> ResultRecord {
    match r {
        Ok(v) => ok_bytes(&v),
        Err(e) => ResultRecord::Err(e),
    }
}

impl HostBindings for CellBindings {
    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        let def = match imports::lookup(call.import) {
            Some(d) => d,
            None => {
                self.ctx.latch_trap(TrapCode::CapabilityViolation);
                return INVALID_FUTURE;
            }
        };
        match def.kind {
            ImportKind::Future => (),
            ImportKind::Sync => {
                self.ctx.latch_trap(TrapCode::Protocol);
                return INVALID_FUTURE;
            }
        };
        let Some(id) = self.alloc(def.name) else {
            return INVALID_FUTURE;
        };

        // In the grace window every import answers `Cancelled` through its
        // future, so an over-budget dispatch cannot spend the window issuing
        // work the host will not do.
        if self.ctx.is_cancelled() {
            return self.settle_now(id, ResultRecord::Err(FutureError::Cancelled));
        }

        let Some(rt) = self.rt().cloned() else {
            return id;
        };
        let ctx = self.ctx.clone();
        let tenant = ctx.tenant.clone();
        let payload = call.payload.to_vec();
        let scalars = call.scalars;

        macro_rules! spawn {
            (|$svc:ident| $body:expr) => {{
                let rt2 = rt.clone();
                rt.reactor.spawn(async move {
                    let $svc = rt2.services.clone();
                    let res: ResultRecord = $body;
                    CellBindings::settle(&rt2, id, res);
                });
                return id;
            }};
        }

        match def.name {
            "clock_deadline_in" => {
                let ms = scalars[0];
                let rt2 = rt.clone();
                rt.reactor.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    CellBindings::settle(&rt2, id, ResultRecord::Ok(Vec::new()));
                });
                id
            }

            "http_fetch" => {
                let Some(req) = self.decode::<FetchRequest>(&payload) else {
                    return self.settle_now(
                        id,
                        ResultRecord::Err(FutureError::Denied("malformed fetch request".into())),
                    );
                };
                if let Err(e) = self.check_subrequest() {
                    rt.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                    return self.settle_now(id, ResultRecord::Err(e));
                }
                // The check blocks on `getaddrinfo`, so it runs on the
                // reactor's blocking pool.
                let allowlist = ctx.allowlist.clone();
                let rt2 = rt.clone();
                rt.reactor.spawn(async move {
                    let svc = rt2.services.clone();
                    let checked = {
                        let egress = svc.egress.clone();
                        let url = req.url.clone();
                        tokio::task::spawn_blocking(move || {
                            egress.check_typed(&url, allowlist.as_deref())
                        })
                        .await
                    };
                    let res = match checked {
                        Ok(Ok(pinned)) => {
                            let out = svc.fetcher.fetch(pinned, req).await;
                            if let Ok(r) = &out {
                                svc.egress.record_bytes(
                                    r.body_inline.as_ref().map_or(0, |b| b.len()) as u64,
                                );
                            }
                            svc.egress.release_connection();
                            from_typed(out)
                        }
                        Ok(Err(deny)) => {
                            tracing::warn!(tenant = %tenant, reason = %deny, "egress denied");
                            rt2.metrics.egress_denials.fetch_add(1, Ordering::Relaxed);
                            ResultRecord::Err(FutureError::Denied(deny.to_string()))
                        }
                        Err(e) => ResultRecord::Err(FutureError::Io(format!("egress check: {e}"))),
                    };
                    CellBindings::settle(&rt2, id, res);
                });
                id
            }
            "http_body_read" => {
                let handle = scalars[0] as u32;
                let max = scalars[1] as usize;
                spawn!(|svc| from_typed(svc.fetcher.read_body(handle, max).await))
            }

            "resp_write" => {
                let handle = scalars[0] as u32;
                let res = match ctx.responses.lock().write(handle, payload) {
                    Ok(n) => {
                        ResultRecord::Ok(postcard::to_allocvec(&(n as u32)).unwrap_or_default())
                    }
                    Err(e) => ResultRecord::Err(FutureError::Io(format!("resp_write: {e:?}"))),
                };
                self.settle_now(id, res)
            }

            other => {
                tracing::error!(import = other, "future-class import with no route");
                self.settle_now(
                    id,
                    ResultRecord::Err(FutureError::Denied(format!("unrouted import {other}"))),
                )
            }
        }
    }

    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        let Some(def) = imports::lookup(call.import) else {
            self.ctx.latch_trap(TrapCode::CapabilityViolation);
            return 0;
        };
        if matches!(def.kind, ImportKind::Future) {
            self.ctx.latch_trap(TrapCode::Protocol);
            return 0;
        }
        let ctx = &self.ctx;

        match def.name {
            "abort" => {
                let msg = String::from_utf8_lossy(call.payload).to_string();
                tracing::warn!(tenant = %ctx.tenant, instance = %ctx.instance, %msg, "guest abort");
                ctx.latch_trap(TrapCode::Panic);
                0
            }
            // At runtime every init unit has already run or been deferred.
            "init_unit_begin" => 0,
            "init_unit_deferred" => 0,

            // Log CPU is already on the dispatch's meter; the interval makes
            // the share visible and the byte cap bounds volume.
            "log_emit" => {
                let cpu = CpuInterval::start();
                let level = call.scalars[0] as u8;
                let n = call.payload.len() as u32;
                let used = ctx.log_bytes.fetch_add(n, Ordering::Relaxed) + n;
                if used > MAX_LOG_BYTES_PER_DISPATCH {
                    return 0;
                }
                let fields = String::from_utf8_lossy(call.payload).to_string();
                tracing::info!(
                    tenant = %ctx.tenant,
                    module = %ctx.module,
                    instance = %ctx.instance,
                    dispatch = ctx.dispatch_id(),
                    at_ms = ctx.slice_clock_ms(),
                    level,
                    %fields,
                    "guest log"
                );
                ctx.meters.record_log_cpu(cpu.elapsed_ns());
                0
            }
            "trace_span_begin" => {
                let name = String::from_utf8_lossy(call.payload).to_string();
                let id = ctx.next_span.fetch_add(1, Ordering::Relaxed) as i32;
                ctx.spans.lock().insert(id, (name, ctx.slice_clock_ms()));
                id as i64
            }
            "trace_span_end" => {
                let id = call.scalars[0] as i32;
                if let Some((name, at)) = ctx.spans.lock().remove(&id) {
                    tracing::debug!(tenant = %ctx.tenant, span = %name, began_ms = at, "span end");
                }
                0
            }

            "clock_now_coarse" => ctx.slice_clock_ms() as i64,

            // Out-buffer imports belong to `call_fill`; here there is nowhere
            // to put the bytes, and a zero-filled buffer would be a constant
            // CSPRNG.
            "entropy_fill" | "init_hash_seed" | "init_config_get" => {
                ctx.latch_trap(TrapCode::Protocol);
                -1
            }

            "resp_new" => {
                let Some(head) = self.decode::<ResponseHead>(call.payload) else {
                    return 0;
                };
                ctx.responses.lock().create(head) as i64
            }
            "resp_finish" => {
                let h = call.scalars[0] as u32;
                match ctx.responses.lock().finish(h) {
                    Ok(()) => 0,
                    Err(_) => -1,
                }
            }

            // Answered by the engine before reaching this vtable; an engine
            // that does not intercept them gets "budget untouched, bit clear"
            // rather than a capability violation for a declared import.
            "budget_remaining_ns" => i64::MAX,
            "unsafe_to_trap" => 0,

            other => {
                tracing::error!(import = other, "sync import with no route");
                ctx.latch_trap(TrapCode::CapabilityViolation);
                0
            }
        }
    }

    /// The host writes into a host-side buffer and the engine copies it in, so
    /// no host pointer enters guest memory.
    fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> i64 {
        match call.import {
            "entropy_fill" => {
                if out.len() > imports::MAX_OUT_BUFFER_BYTES {
                    return -1;
                }
                rand::rng().fill_bytes(out);
                self.ctx.entropy_calls.fetch_add(1, Ordering::Relaxed);
                *self.ctx.out_buffer.lock() = out.to_vec();
                out.len() as i64
            }
            // Served in the sealed init sandbox by the control plane; at
            // runtime the guest already holds the config in linear memory.
            "init_hash_seed" | "init_config_get" => {
                self.ctx.latch_trap(TrapCode::CapabilityViolation);
                -1
            }
            other => {
                tracing::error!(import = other, "fill import with no route");
                self.ctx.latch_trap(TrapCode::Protocol);
                -1
            }
        }
    }

    /// Poisons the imports and completes the cancellation future. Runs inside
    /// the epoch callback with the guest on the stack; completing a future
    /// signals a wake that the reentrancy guard holds until this call returns.
    fn enter_grace(&mut self) {
        if !self.ctx.poison_imports() {
            return;
        }
        let cancel = self.ctx.cancel_future();
        tracing::debug!(
            tenant = %self.ctx.tenant, instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(), cancel_future = cancel,
            "dispatch entered its grace window: imports poisoned, cancellation tripped"
        );
        if cancel != 0 {
            self.settle_now(cancel, ResultRecord::Err(FutureError::Cancelled));
        }
    }

    fn slice_clock_ms(&self) -> u64 {
        self.ctx.slice_clock_ms()
    }
}

const MAX_LOG_BYTES_PER_DISPATCH: u32 = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::imports::IMPORTS;

    /// Scalars an import carries, per the contract crate's `BufSpec`.
    fn scalar_count(def: &imports::ImportDef) -> usize {
        match def.buf {
            imports::BufSpec::None => def.sig.params.len(),
            imports::BufSpec::In(_) | imports::BufSpec::Out(_) => {
                def.sig.params.len().saturating_sub(2)
            }
        }
    }

    fn ctx() -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::for_test(
            InstanceId(1),
            HandlerLifecycle::Reuse,
            Limits::default(),
        ))
    }

    fn call<'a>(import: &'static str, payload: &'a [u8], scalars: [u64; 3]) -> ImportCall<'a> {
        ImportCall {
            import,
            payload,
            scalars,
        }
    }

    #[test]
    fn scalar_layout_matches_the_abi() {
        for (name, _) in SCALARS {
            assert!(
                imports::lookup(name).is_some(),
                "SCALARS names unknown import {name}"
            );
        }
        for def in IMPORTS {
            let n = scalar_count(def);
            let documented = SCALARS
                .iter()
                .find(|(m, _)| *m == def.name)
                .map(|(_, s)| s.len());
            match documented {
                Some(named) => assert_eq!(
                    named, n,
                    "{} carries {n} scalar(s) per BufSpec but SCALARS names {named}",
                    def.name
                ),
                None => assert_eq!(
                    n, 0,
                    "{} carries {n} scalar(s) that SCALARS does not name",
                    def.name
                ),
            }
        }
    }

    #[test]
    fn every_out_buffer_import_is_routed_through_call_fill() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut out = [0u8; 64];
        for def in IMPORTS
            .iter()
            .filter(|d| matches!(d.buf, imports::BufSpec::Out(_)))
        {
            let n = b.call_fill(call(def.name, &[], [0; 3]), &mut out);
            let _ = c.take_trap();
            assert!(
                n >= 0 || matches!(def.name, "init_hash_seed" | "init_config_get"),
                "{} has an out buffer but call_fill refuses it",
                def.name
            );
        }
    }

    #[test]
    fn the_coarse_clock_is_frozen_within_a_slice() {
        let c = ctx();
        c.begin_dispatch(1_000, 1);
        let mut b = CellBindings::new(c.clone());
        let a = b.call_sync(call("clock_now_coarse", &[], [0; 3]));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let z = b.call_sync(call("clock_now_coarse", &[], [0; 3]));
        assert_eq!(a, z, "the imported coarse clock advanced inside a slice");
        assert_eq!(a, 1_000, "the guest reads the frozen slice clock");
        assert_eq!(b.slice_clock_ms(), 1_000, "and so do the bindings");

        c.begin_slice(1_007, 1);
        assert_eq!(
            b.call_sync(call("clock_now_coarse", &[], [0; 3])),
            1_007,
            "the clock advances at the slice boundary and only there"
        );
    }

    #[test]
    fn the_coarse_clock_never_goes_backwards() {
        let c = ctx();
        c.begin_slice(5_000, 1);
        c.begin_slice(4_000, 2);
        assert_eq!(
            c.slice_clock_ms(),
            5_000,
            "a host clock stepping backwards is not visible in the sandbox"
        );
    }

    #[test]
    fn entropy_is_drawn_per_call_and_never_cached() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut seen = std::collections::HashSet::new();
        let mut buf = [0u8; 32];
        for _ in 0..64 {
            let n = b.call_fill(call("entropy_fill", &[], [0; 3]), &mut buf);
            assert_eq!(n, 32, "the whole buffer is filled");
            assert!(buf.iter().any(|x| *x != 0), "entropy returned zeros");
            assert!(
                seen.insert(buf.to_vec()),
                "entropy repeated: it was cached somewhere"
            );
        }
        assert_eq!(c.entropy_calls(), 64, "every draw is counted");
    }

    #[test]
    fn an_out_buffer_larger_than_the_cap_is_refused() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut big = vec![0u8; imports::MAX_OUT_BUFFER_BYTES + 1];
        assert_eq!(
            b.call_fill(call("entropy_fill", &[], [0; 3]), &mut big),
            -1,
            "a fill past the out-buffer cap is refused"
        );
        assert!(big.iter().all(|x| *x == 0), "and nothing was written");
        assert_eq!(c.entropy_calls(), 0, "and no draw was counted");
    }

    #[test]
    fn entropy_through_a_path_with_no_out_buffer_is_denied() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        assert_eq!(
            b.call_sync(call("entropy_fill", &[], [32, 0, 0])),
            -1,
            "the sync path has nowhere to put the bytes"
        );
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::Protocol),
            "and latches a protocol trap"
        );
    }

    #[test]
    fn clones_do_not_share_an_rng_stream() {
        let a = Arc::new(InstanceCtx::for_test(
            InstanceId(1),
            HandlerLifecycle::Reuse,
            Limits::default(),
        ));
        let z = Arc::new(InstanceCtx::for_test(
            InstanceId(2),
            HandlerLifecycle::Reuse,
            Limits::default(),
        ));
        let mut ba = CellBindings::new(a);
        let mut bz = CellBindings::new(z);
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        ba.call_fill(call("entropy_fill", &[], [0; 3]), &mut x);
        bz.call_fill(call("entropy_fill", &[], [0; 3]), &mut y);
        assert_ne!(x, y, "two clones drew the same bytes");
    }

    #[test]
    fn the_deploy_hash_seed_is_not_reachable_at_runtime() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut seed = [0u8; 16];
        assert_eq!(
            b.call_fill(call("init_hash_seed", &[], [0; 3]), &mut seed),
            -1,
            "the init seed is refused at runtime"
        );
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::CapabilityViolation),
            "and is a capability violation"
        );
        assert_eq!(
            seed, [0u8; 16],
            "the seed was written into guest memory at runtime"
        );
    }

    #[test]
    fn abort_latches_a_panic_trap() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("abort", b"boom", [0; 3]));
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::Panic),
            "a guest abort is a panic trap"
        );
    }

    #[test]
    fn logs_are_structured_charged_and_bounded() {
        let output = TestLogCapture::default();
        let subscriber = output.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&subscriber);
        let c = ctx();
        c.begin_dispatch(10, 1);
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("log_emit", b"{\"msg\":\"hello\"}", [2, 0, 0]));
        assert_eq!(output.records().len(), 1, "the line is emitted");
        assert_eq!(output.records()[0].level, 2, "with its level");

        let big = vec![b'x'; 64 * 1024];
        for _ in 0..8 {
            b.call_sync(call("log_emit", &big, [2, 0, 0]));
        }
        assert!(
            output.records().len() < 9,
            "the per-dispatch log byte cap did not bite"
        );
        c.begin_dispatch(11, 2);
        b.call_sync(call("log_emit", b"after", [2, 0, 0]));
        assert!(
            output.records().iter().any(|l| l.fields == "after"),
            "a new dispatch gets a fresh allowance"
        );
    }

    #[test]
    fn responses_are_built_through_the_vtable() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let head = ResponseHead {
            status: 201,
            headers: vec![],
            body: b"hi".to_vec(),
            streaming: false,
        };
        let h = b.call_sync(call(
            "resp_new",
            &postcard::to_allocvec(&head).expect("encode"),
            [0; 3],
        ));
        assert_eq!(h, 1, "the first response handle");
        assert_eq!(
            b.call_sync(call("resp_finish", &[], [h as u64, 0, 0])),
            0,
            "finishing a live handle succeeds"
        );
        assert_eq!(
            b.call_sync(call("resp_finish", &[], [99, 0, 0])),
            -1,
            "finishing an unknown handle is refused"
        );
        let r = c.responses.lock().take(h as u32).expect("the response");
        assert_eq!(r.head.status, 201, "the head is the guest's");
        assert!(r.finished, "and it is finished");
    }

    #[test]
    fn the_two_call_paths_do_not_accept_each_others_imports() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("clock_deadline_in", &[], [0; 3]));
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::Protocol),
            "a future import through the sync path is a protocol error"
        );
        assert_eq!(
            b.call_future(call("clock_now_coarse", &[], [0; 3])),
            INVALID_FUTURE,
            "a sync import through the future path allocates nothing"
        );
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::Protocol),
            "and is a protocol error"
        );
    }

    #[derive(Clone, Default)]
    struct TestLogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    #[derive(Debug, serde::Deserialize)]
    struct TestGuestLog {
        level: u8,
        fields: String,
    }

    impl TestLogCapture {
        fn dispatch(&self) -> tracing::Dispatch {
            tracing::Dispatch::new(
                tracing_subscriber::fmt()
                    .json()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::TRACE)
                    .with_writer(self.clone())
                    .finish(),
            )
        }

        fn records(&self) -> Vec<TestGuestLog> {
            let bytes = self.0.lock().expect("test writer mutex").clone();
            String::from_utf8(bytes)
                .expect("tracing emits UTF-8")
                .lines()
                .filter_map(|line| {
                    let event: serde_json::Value =
                        serde_json::from_str(line).expect("tracing JSON event");
                    (event["fields"]["message"] == "guest log").then(|| {
                        serde_json::from_value(event["fields"].clone())
                            .expect("the guest logging boundary emits level and text")
                    })
                })
                .collect()
        }
    }

    impl std::io::Write for TestLogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("test writer mutex")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TestLogCapture {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }
}
