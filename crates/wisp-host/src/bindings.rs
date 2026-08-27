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

/// What each import's `scalars` entries *mean*.
///
/// `wisp_abi::imports::ImportDef::buf` is the authority on *where* the
/// `(ptr, len)` pair sits in the parameter list; this table only names what is
/// left over, so the vtable's `call.scalars[0]` reads are legible instead of
/// magic. `scalar_layout_matches_the_abi` checks the two agree, so a new
/// import cannot land with an undocumented scalar.
pub const SCALARS: &[(&str, &[&str])] = &[
    ("log_emit", &["level"]),
    ("trace_span_end", &["span"]),
    ("clock_deadline_in", &["ms"]),
    ("resp_write", &["response_handle"]),
    ("resp_finish", &["response_handle"]),
    ("http_body_read", &["body_handle", "max_len"]),
    ("init_unit_begin", &["unit"]),
    ("init_unit_deferred", &["unit"]),
    // §4.6's kill-cooperation ABI. Both are answered by the engine, so
    // neither reaches this vtable — but they are declared imports and the
    // drift check below covers the whole table, not the part this file
    // happens to route.
    ("unsafe_to_trap", &["on"]),
];

/// Scalars an import carries, per `wisp_abi`'s own `BufSpec`. The vtable does
/// not need this at runtime — the engine has already split the arguments — but
/// `scalar_layout_matches_the_abi` uses it to prove [`SCALARS`] has not
/// drifted from the contract.
#[cfg_attr(not(test), allow(dead_code))]
fn scalar_count(def: &imports::ImportDef) -> usize {
    match def.buf {
        imports::BufSpec::None => def.sig.params.len(),
        imports::BufSpec::In(_) | imports::BufSpec::Out(_) => {
            def.sig.params.len().saturating_sub(2)
        }
    }
}

/// How a completed host future gets back to an execution-pool thread.
/// Implemented by [`crate::Cell`]; a no-op in unit tests.
pub trait WakeSink: Send + Sync + 'static {
    fn signal(&self, owner: InstanceId);

    /// [`WakeSink::signal`], carrying the readings the completion path took
    /// before it got here **and the dispatch they belong to**.
    ///
    /// A separate method with a default rather than extra parameters on
    /// `signal`, so that a sink which has nothing to do with tracing — and
    /// there are three — does not have to mention the instrument at all. Only
    /// the cell's own sink overrides it.
    ///
    /// `dispatch` is the dispatch that **created the future**, not the one
    /// running when it completed. Those differ, and the difference is the
    /// point: a wake is addressed to an instance, but the readings it carries
    /// — and the trace they would be filed into — belong to the request that
    /// awaited it. See [`crate::metrics::Metrics::stale_wakes`].
    fn signal_traced(
        &self,
        owner: InstanceId,
        dispatch: u64,
        stamps: Option<std::sync::Arc<crate::marks::WakeStamps>>,
    ) {
        let _ = (dispatch, stamps);
        self.signal(owner);
    }
}

pub struct NoWake;
impl WakeSink for NoWake {
    fn signal(&self, _owner: InstanceId) {}
}

/// Outbound HTTP, behind a seam because the pinned-address connect is the
/// security-relevant half and must be testable without a network.
#[async_trait::async_trait]
pub trait Fetcher: Send + Sync + 'static {
    /// `target.addr` is the **pinned** address from the SSRF check. An
    /// implementation that re-resolves `target.url` has thrown away §8.6's
    /// rebinding control.
    async fn fetch(
        &self,
        target: wisp_abi::host::PinnedTarget,
        req: FetchRequest,
    ) -> Result<FetchResponse, FutureError>;

    /// Streaming body read (§6.8 Unbounded).
    async fn read_body(&self, handle: u32, max_len: usize) -> Result<BodyChunk, FutureError>;
}

/// Everything the vtable routes to. Assembled by the cell from the control
/// plane's services (§9.1's cell/control-plane seam).
pub struct Services {
    pub egress: Arc<HostEgressPolicy>,
    pub fetcher: Arc<dyn Fetcher>,
}

/// The per-cell machinery an instance's bindings need. Shared by every
/// instance in the cell; per-instance state lives in [`InstanceCtx`].
pub struct CellRuntime {
    pub services: Arc<Services>,
    pub futures: Arc<FutureTable>,
    pub metrics: Arc<Metrics>,
    pub wake: Arc<dyn WakeSink>,
    /// The reactor. §6.1: reactor threads own **all** I/O; an execution-pool
    /// thread never awaits anything, it only returns.
    pub reactor: tokio::runtime::Handle,
    /// A copy of [`crate::cell::CellConfig::stage_sample_every`], so a reactor
    /// thread can decide whether this completion's path into the cell is worth
    /// measuring without reaching back into the cell for the answer.
    ///
    /// Zero is off, and off is the common case: this is the only thing between
    /// an untraced cell and two `RDTSCP`s on every completion.
    pub stage_sample_every: u64,
}

impl CellRuntime {
    /// **A host future has completed; route the wake, and time the path.**
    ///
    /// One function rather than two, and that is the point of it existing.
    /// There are two ways a completion reaches the cell — the vtable's own
    /// `settle`, which is every future a guest awaits, and
    /// `Cell::complete_future`, which is the control plane, the proxy and the
    /// tests — and they used to be two copies of the same three lines. The
    /// first version of this instrument measured only the second, so a real
    /// run reported the pause's internals on 94 requests out of 9,310 and the
    /// rest arrived carrying nothing. Nothing about that looked wrong in the
    /// table; the counts simply differed.
    ///
    /// **The two readings before the sampling decision are unavoidable.** The
    /// predicate runs on a dispatch id and the dispatch id is inside the future
    /// table, under the lock the second reading brackets — so while
    /// `stage_sample_every` is non-zero, every completion pays two `RDTSCP`s,
    /// about 40 ns. At zero it pays one relaxed load and a branch.
    pub fn settle_future(&self, id: u64, res: wisp_abi::result::ResultRecord) {
        let taking = self.stage_sample_every != 0;
        // The wait for the outside world closes at this instruction, not three
        // locks later where the wake is enqueued.
        let arrived = if taking {
            crate::marks::stamp()
        } else {
            crate::marks::NO_STAMP
        };
        let done = self.futures.complete_marked(id, res, taking);
        let WakeTarget::Instance(owner) = done.target else {
            return;
        };
        // The dispatch id is known now, so the readings can be kept or thrown
        // away. The allocation below is the instrument's own and lands inside
        // the span ending at `WakeIndexWanted`, which says so.
        let stamps = (taking && crate::cell::stage_sampled(done.dispatch, self.stage_sample_every))
            .then(|| {
                let st = Arc::new(crate::marks::WakeStamps::default());
                st.arrived.set(arrived);
                st.complete_acquired.set(done.acquired);
                st.complete_released.take();
                st
            });
        // `done.dispatch` travels with the wake from here. It was already read
        // — the sampling predicate above runs on it — and it is the only place
        // it can be read: the future table knows which dispatch registered the
        // future, and by the time a worker pops the wake the instance may be
        // running a different one.
        self.wake.signal_traced(owner, done.dispatch, stamps);
    }
}

/// The per-instance half of the vtable's state.
///
/// This lives behind an `Arc` shared between the slot and the
/// [`CellBindings`] that was moved into the engine at activation, which is how
/// the cell can freeze the clock, read the trap latch and collect the response
/// for an instance whose bindings it no longer owns.
pub struct InstanceCtx {
    pub instance: InstanceId,
    pub module: ModuleId,
    pub tenant: String,
    pub limits: Limits,
    pub lifecycle: HandlerLifecycle,
    /// §8.6 optional per-app fetch allowlist.
    pub allowlist: Option<Vec<String>>,
    pub meters: Arc<Meters>,
    /// `None` in unit tests that exercise the slot machinery without a cell.
    pub rt: Option<Arc<CellRuntime>>,

    /// §8.5: the frozen slice clock. Written once per slice by the cell,
    /// read by the guest through `clock_now_coarse`.
    clock_ms: AtomicU64,
    /// Current top-level work item; the unit §11.1 caps futures per.
    dispatch: AtomicU64,
    /// §11.1 subrequests per dispatch.
    subrequests: AtomicU32,
    /// Incoming guest log payload bytes charged to this dispatch's allowance.
    log_bytes: AtomicU32,
    /// Counted so a test can assert entropy is drawn per call, never memoized.
    entropy_calls: AtomicU64,
    /// A trap the vtable owes the guest but cannot raise from `call_sync`
    /// (which returns an `i64`). The cell converts it after the call returns.
    trap_latch: Mutex<Option<TrapCode>>,
    /// §6.4/§5.3: **import poisoning**. Set the moment this dispatch enters
    /// its grace window, and cleared at the next `begin_dispatch`. While it is
    /// set, *"every import called by that dispatch returns `Cancelled`
    /// immediately"* — the guest cannot start new work with budget it does not
    /// have, and every `await` it is already sitting on resolves to a terminal
    /// error, which is what gives the unwind something to unwind *for*.
    cancelled: AtomicBool,
    /// The dispatch's implicit cancellation future (§5.4). The host cancels by
    /// completing it; the guest observes a tripped token at its next await.
    /// Cancellation is deliberately **not** an entrancy — it would violate the
    /// single-caller rule against exactly the instance least able to honour it.
    cancel_future: AtomicU64,
    /// Host→guest bytes for sync imports that write into guest memory
    /// (`entropy_fill`). The engine copies this out after the call.
    out_buffer: Mutex<Vec<u8>>,
    pub responses: Mutex<ResponseTable>,
    spans: Mutex<HashMap<i32, (String, u64)>>,
    next_span: AtomicU32,
}

impl InstanceCtx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        instance: InstanceId,
        module: ModuleId,
        tenant: impl Into<String>,
        limits: Limits,
        lifecycle: HandlerLifecycle,
        allowlist: Option<Vec<String>>,
        meters: Arc<Meters>,
        rt: Option<Arc<CellRuntime>>,
    ) -> Self {
        Self {
            instance,
            module,
            tenant: tenant.into(),
            limits: limits,
            tier,
            lifecycle,
            allowlist,
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
            ModuleId::new("test", "v1"),
            "t",
            limits,
            tier,
            lifecycle,
            None,
            Arc::new(Meters::new()),
            None,
        )
    }

    /// §8.5: freeze the coarse clock for one slice, monotonic per instance.
    ///
    /// Called by the cell immediately before entering the guest and not again
    /// until the next dispatch or wake. Within a slice the guest sees one
    /// value on every coarse-clock read.
    pub fn begin_slice(&self, now_ms: u64, dispatch: u64) {
        // A host clock adjustment cannot make the exposed clock go backwards.
        let prev = self.clock_ms.load(Ordering::Relaxed);
        self.clock_ms.store(now_ms.max(prev), Ordering::Relaxed);
        self.dispatch.store(dispatch, Ordering::Relaxed);
    }

    /// Reset the per-dispatch counters §11.1 caps.
    pub fn begin_dispatch(&self, now_ms: u64, dispatch: u64) {
        self.subrequests.store(0, Ordering::Relaxed);
        self.log_bytes.store(0, Ordering::Relaxed);
        // A new dispatch is not cancelled, whatever happened to the last one.
        // Under `reuse` this instance may well be one that was cleanly killed
        // a moment ago; leaving the poison set would make the grace window a
        // one-way door for the slot.
        self.cancelled.store(false, Ordering::Relaxed);
        self.cancel_future.store(0, Ordering::Relaxed);
        self.begin_slice(now_ms, dispatch);
    }

    /// §5.4: the dispatch's implicit cancellation future, allocated by the
    /// cell before the guest is entered and carried in the `DispatchRecord`.
    pub fn set_cancel_future(&self, id: u64) {
        self.cancel_future.store(id, Ordering::Relaxed);
    }

    pub fn cancel_future(&self) -> u64 {
        self.cancel_future.load(Ordering::Relaxed)
    }

    /// §6.4: has this dispatch entered grace, i.e. are its imports poisoned?
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Enter the grace window: poison this dispatch's imports (§5.3).
    ///
    /// Returns false if it was already set, so the caller can complete the
    /// cancellation future exactly once.
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
    /// Is a trap latched, without consuming it?
    ///
    /// [`InstanceCtx::take_trap`] is destructive, and the decision that needs
    /// this is taken *before* the one that consumes it: the cell must know
    /// whether this call is going to end in a tombstone while it still holds
    /// the sandbox, because releasing the sandbox first opens a window in which
    /// another thread can enter an instance that is about to be destroyed. See
    /// `Cell::settle`.
    pub fn has_trap(&self) -> bool {
        self.trap_latch.lock().is_some()
    }
    pub fn latch_trap(&self, code: TrapCode) {
        let mut g = self.trap_latch.lock();
        if g.is_none() {
            *g = Some(code);
        }
    }
    /// See the module docs on `entropy_fill`: the engine copies these bytes
    /// into guest memory at `(ptr, len)` after the sync call returns.
    pub fn take_out_buffer(&self) -> Vec<u8> {
        std::mem::take(&mut self.out_buffer.lock())
    }
}

/// The vtable itself. One per instance; moved into the engine at activation.
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
    /// Host→guest bytes for `entropy_fill`. See the module docs.
    pub fn take_out_buffer(&mut self) -> Vec<u8> {
        self.ctx.take_out_buffer()
    }

    fn rt(&self) -> Option<&Arc<CellRuntime>> {
        self.ctx.rt.as_ref()
    }

    /// Register a future for this instance, honouring §11.1's cap.
    ///
    /// Refusal returns [`INVALID_FUTURE`] and latches
    /// [`TrapCode::TooManyFutures`], because `call_future` returns a bare
    /// `u64` and cannot trap directly. The cell converts the latch into a trap
    /// as soon as the call returns, so the guest cannot proceed as though the
    /// call succeeded.
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
                    limit = e.limit, "pending-futures cap exceeded (§11.1)"
                );
                self.ctx.latch_trap(TrapCode::TooManyFutures);
                None
            }
        }
    }

    /// Complete a future from a reactor thread and signal the wake router.
    ///
    /// **The** completion path for everything a guest awaits, and the reason
    /// [`CellRuntime::settle_future`] exists rather than three lines here: this
    /// and `Cell::complete_future` were two copies, and instrumenting only the
    /// second measured 1% of the pauses in a real run.
    fn settle(rt: &Arc<CellRuntime>, id: u64, res: ResultRecord) {
        rt.settle_future(id, res);
    }

    /// Resolve a future immediately, without touching the reactor. Used for
    /// policy refusals: the guest still learns through the future, which keeps
    /// the ABI's "there is no import that waits" rule intact (§5.1).
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

    /// §11.1 subrequests per dispatch, and §8.6's egress policy.
    fn check_subrequest(&self) -> Result<(), FutureError> {
        let n = self.ctx.subrequests.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.ctx.limits.subrequests_per_dispatch {
            return Err(FutureError::Denied(format!(
                "subrequest cap of {} per dispatch exceeded (§11.1)",
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
                // Cannot happen: the linker defines only `IMPORTS`. Rule V1
                // enforced twice (§4.3) — this is the second time.
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

        // §5.3, import poisoning: *"During a dispatch's grace window (§6.4)
        // every import called by that dispatch returns `Cancelled`
        // immediately"*. The refusal still arrives through the future, which
        // keeps §5.1's "there is no import that waits" intact — the guest gets
        // a future id back and learns the answer at its next poll, exactly as
        // it would for an SSRF denial.
        //
        // This is what makes a clean kill *possible* rather than merely
        // permitted: an over-budget dispatch that keeps calling `kv_get` in a
        // retry loop would otherwise consume its whole grace window issuing
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
            // ---- clock -----------------------------------------------------
            // A timer is a host future, not a guest loop (§5.3). It costs the
            // platform a timer entry and the instance zero CPU while it waits,
            // which is the whole of G2.
            "clock_deadline_in" => {
                let ms = scalars[0];
                let rt2 = rt.clone();
                rt.reactor.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    CellBindings::settle(&rt2, id, ResultRecord::Ok(Vec::new()));
                });
                id
            }

            // ---- http ------------------------------------------------------
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
                // SSRF policy, host-side, resolve-then-pin (§8.6). The check
                // runs on the reactor because `getaddrinfo` blocks and an
                // execution-pool thread must never block.
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
                            tracing::warn!(tenant = %tenant, reason = %deny, "egress denied (§8.6)");
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

            // ---- response streaming (§6.3) ---------------------------------
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
            // ---- core -------------------------------------------------------
            "abort" => {
                let msg = String::from_utf8_lossy(call.payload).to_string();
                tracing::warn!(tenant = %ctx.tenant, instance = %ctx.instance, %msg, "guest abort");
                ctx.latch_trap(TrapCode::Panic);
                0
            }
            // The init-sandbox handshake (§4.4) only means anything inside
            // pre-init, which is `wisp-control`'s job. At runtime every unit
            // has already run or been deferred, so nothing is deferred here.
            "init_unit_begin" => 0,
            "init_unit_deferred" => 0,

            // ---- observability (§10) -----------------------------------------
            // "Log CPU counts against the dispatch budget (prevents
            // log-as-compute abuse)." It does so automatically: this runs on
            // the execution-pool thread inside the dispatch's Hot interval, so
            // `CLOCK_THREAD_CPUTIME_ID` is already ticking. The measurement
            // here exists to make the share visible (§10), and the byte cap
            // bounds volume independently of CPU.
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

            // ---- clock (§8.4/§8.5) --------------------------------------------
            // Clock reads use the value fixed at slice entry.
            "clock_now_coarse" => ctx.slice_clock_ms() as i64,

            // ---- entropy (§8.5) -----------------------------------------------
            // Host→guest, so it goes through `call_fill`, which is the only
            // path with somewhere to put the bytes. Reaching it here means the
            // engine linked it as an ordinary sync import; deny rather than
            // return a zero-filled buffer, because a silently constant
            // "CSPRNG" would break §8.5 invisibly.
            "entropy_fill" | "init_hash_seed" | "init_config_get" => {
                ctx.latch_trap(TrapCode::Protocol);
                -1
            }

            // ---- response building (§6.3) -------------------------------------
            "resp_new" => {
                let Some(head) = self.decode::<ResponseHead>(call.payload) else {
                    return 0;
                };
                ctx.responses.lock().create(head) as i64
            }
            "resp_finish" => {
                let h = call.scalars[0] as u32;
                let _ = ctx.responses.lock().finish(h);
                0
            }

            // §4.6's kill-cooperation ABI is answered by the ENGINE, next to
            // the meter it reads and the epoch callback it feeds, and never
            // reaches this vtable. These arms exist so that a host running
            // against a fake or future engine that does not intercept them
            // gets a safe answer — "budget untouched, bit clear" — instead of
            // a capability violation for calling a declared import.
            "budget_remaining_ns" => i64::MAX,
            "unsafe_to_trap" => 0,

            other => {
                tracing::error!(import = other, "sync import with no route");
                ctx.latch_trap(TrapCode::CapabilityViolation);
                0
            }
        }
    }

    /// Host→guest sync imports (§5.5: the host writes into a host-side buffer
    /// and the engine copies it in, so no host pointer enters guest memory).
    fn call_fill(&mut self, call: ImportCall<'_>, out: &mut [u8]) -> i64 {
        match call.import {
            // Each call draws from the host CSPRNG without caching its output.
            // Host generator state stays outside captured guest memory; guest
            // code can retain returned bytes or its own PRNG state.
            "entropy_fill" => {
                if out.len() > MAX_ENTROPY_BYTES as usize {
                    return -1;
                }
                rand::rng().fill_bytes(out);
                self.ctx.entropy_calls.fetch_add(1, Ordering::Relaxed);
                // Mirrored for callers that cannot take an out-slice; see
                // `InstanceCtx::take_out_buffer`.
                *self.ctx.out_buffer.lock() = out.to_vec();
                out.len() as i64
            }
            // §4.4/§8.6: both of these belong to the sealed init sandbox, and
            // `wisp-control` serves them there. At runtime they are a
            // capability violation, not a value.
            //
            // `init_config_get` is safe to *pre-run* — it is tenant-authored
            // deploy input, fixed for the version and part of §8.7's
            // reproducibility preimage, so unlike a secret it bakes no shared
            // entropy into the template. But that is an argument for reading
            // it at init, not for reading it later: the guest captured the
            // blob into linear memory during pre-init and serves it from there
            // forever after, so a runtime call means something has gone wrong.
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

    /// §6.4: the dispatch has crossed B. Two things, both required:
    ///
    /// 1. **poison the imports** (§5.3) — from here every future-returning
    ///    import this dispatch makes answers `Cancelled` immediately;
    /// 2. **complete the cancellation future** (§5.4) — the guest observes a
    ///    tripped token at its next await, which is the only way a dispatch
    ///    sitting on an `await` can learn about a kill, because cancellation
    ///    is not an entrancy.
    ///
    /// Runs inside the epoch deadline callback with the guest on the stack, so
    /// it must not re-enter the instance. It does not: completing a future
    /// marks it ready and signals the wake router, and §5.4's reentrancy guard
    /// makes that wake wait for the current call to return.
    fn enter_grace(&mut self) {
        if !self.ctx.poison_imports() {
            return;
        }
        let cancel = self.ctx.cancel_future();
        tracing::debug!(
            tenant = %self.ctx.tenant, instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(), cancel_future = cancel,
            "dispatch entered its grace window: imports poisoned, cancellation tripped (§6.4)"
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
const MAX_ENTROPY_BYTES: u64 = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::imports::IMPORTS;

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

    /// Every import's scalars are named, and the count matches `wisp_abi`'s
    /// own `BufSpec`. If a new import lands with an undocumented scalar, or
    /// the ABI moves a buffer, this fails here rather than at runtime as an
    /// argument read from the wrong slot.
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

    /// §5.5: host bytes reach guest memory through `call_fill` and nowhere
    /// else, so every `BufSpec::Out` import must be routed there.
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

    /// Repeated coarse-clock reads return one fixed value within a slice.
    #[test]
    fn the_coarse_clock_is_frozen_within_a_slice() {
        let c = ctx();
        c.begin_dispatch(1_000, 1);
        let mut b = CellBindings::new(c.clone());
        let a = b.call_sync(call("clock_now_coarse", &[], [0; 3]));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let z = b.call_sync(call("clock_now_coarse", &[], [0; 3]));
        assert_eq!(a, z, "the imported coarse clock advanced inside a slice");
        assert_eq!(a, 1_000);
        assert_eq!(b.slice_clock_ms(), 1_000);

        // It advances at the next slice boundary, and only there.
        c.begin_slice(1_007, 1);
        assert_eq!(b.call_sync(call("clock_now_coarse", &[], [0; 3])), 1_007);
    }

    /// §8.5: monotonic per instance. A host clock that steps backwards must
    /// not be visible as time going backwards inside the sandbox.
    #[test]
    fn the_coarse_clock_never_goes_backwards() {
        let c = ctx();
        c.begin_slice(5_000, 1);
        c.begin_slice(4_000, 2);
        assert_eq!(c.slice_clock_ms(), 5_000);
    }

    /// Entropy requests reach the host generator per call without cached answers.
    /// This does not inspect guest-owned values or PRNG state in the template.
    #[test]
    fn entropy_is_drawn_per_call_and_never_cached() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut seen = std::collections::HashSet::new();
        let mut buf = [0u8; 32];
        for _ in 0..64 {
            let n = b.call_fill(call("entropy_fill", &[], [0; 3]), &mut buf);
            assert_eq!(n, 32);
            assert!(buf.iter().any(|x| *x != 0), "entropy returned zeros");
            assert!(
                seen.insert(buf.to_vec()),
                "entropy repeated: it was cached somewhere"
            );
        }
        assert_eq!(c.entropy_calls(), 64);
    }

    /// The engine's `call_fill` default denies rather than zero-filling, and
    /// so does this: a sync path with nowhere to put the bytes must not
    /// pretend to have succeeded (§8.5).
    #[test]
    fn entropy_through_a_path_with_no_out_buffer_is_denied() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        assert_eq!(b.call_sync(call("entropy_fill", &[], [32, 0, 0])), -1);
        assert_eq!(c.take_trap(), Some(TrapCode::Protocol));
    }

    /// Two instances cloned from the same template must not share a stream.
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
        assert_ne!(x, y);
    }

    /// §8.6: `secret_get` is runtime-only, and the init-sandbox seed is not
    /// reachable at runtime.
    #[test]
    fn the_deploy_hash_seed_is_not_reachable_at_runtime() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut seed = [0u8; 16];
        assert_eq!(
            b.call_fill(call("init_hash_seed", &[], [0; 3]), &mut seed),
            -1
        );
        assert_eq!(c.take_trap(), Some(TrapCode::CapabilityViolation));
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
        assert_eq!(c.take_trap(), Some(TrapCode::Panic));
    }

    /// §10: a log line costs CPU on the dispatch's own thread, and volume is
    /// capped so log-as-compute cannot be turned into log-as-storage either.
    #[test]
    fn logs_are_structured_charged_and_bounded() {
        let output = TestLogCapture::default();
        let subscriber = output.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&subscriber);
        let c = ctx();
        c.begin_dispatch(10, 1);
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("log_emit", b"{\"msg\":\"hello\"}", [2, 0, 0]));
        assert_eq!(output.records().len(), 1);
        assert_eq!(output.records()[0].level, 2);

        let big = vec![b'x'; 64 * 1024];
        for _ in 0..8 {
            b.call_sync(call("log_emit", &big, [2, 0, 0]));
        }
        assert!(
            output.records().len() < 9,
            "the per-dispatch log byte cap did not bite"
        );
        // A new dispatch gets a fresh allowance.
        c.begin_dispatch(11, 2);
        b.call_sync(call("log_emit", b"after", [2, 0, 0]));
        assert!(output.records().iter().any(|l| l.fields == "after"));
    }

    /// Response building is synchronous; streaming chunks are future-shaped
    /// because a stream cannot be buffered across hibernation (§6.3, §6.8).
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
            &postcard::to_allocvec(&head).unwrap(),
            [0; 3],
        ));
        assert_eq!(h, 1);
        b.call_sync(call("resp_finish", &[], [h as u64, 0, 0]));
        let r = c.responses.lock().take(h as u32).unwrap();
        assert_eq!(r.head.status, 201);
        assert!(r.finished);
    }

    /// Calling a future-class import through the sync path (or vice versa) is
    /// a protocol error, not something the host guesses about.
    #[test]
    fn the_two_call_paths_do_not_accept_each_others_imports() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("clock_deadline_in", &[], [0; 3]));
        assert_eq!(c.take_trap(), Some(TrapCode::Protocol));
        assert_eq!(
            b.call_future(call("clock_now_coarse", &[], [0; 3])),
            INVALID_FUTURE
        );
        assert_eq!(c.take_trap(), Some(TrapCode::Protocol));
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
