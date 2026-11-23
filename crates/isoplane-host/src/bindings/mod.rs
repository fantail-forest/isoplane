//! The import vtable routes guest capabilities through host-owned services.
//! `clock_now_coarse` is frozen within a slice; `entropy_fill` draws from the
//! host CSPRNG per call.
//!
//! `ImportCall::scalars` carries the non-buffer scalar arguments in
//! declaration order, the `(ptr, len)` pair stripped.
//!
//! `call_future` and `call_sync` are routers: each import's arms live in a
//! module named for the capability it belongs to (`http`,
//! `sockets`, `observability`); a capability with no state or subrequest to
//! coordinate (the clock, entropy and `abort`) stays here as a sandbox
//! primitive.
//!
//! The imports only a Rust guest calls are routed with the `rust-guests`
//! feature, and their modules are compiled only with it.

mod cancellation;
mod dispatch;
mod http;
#[cfg(feature = "rust-guests")]
mod http_scope;
mod instance;
mod native_io;
mod observability;
#[cfg(feature = "rust-guests")]
mod outbound_http;
mod preparation;
pub(crate) mod request_body;
mod runtime;
mod runtime_configuration;
#[cfg(feature = "rust-guests")]
mod trace_spans;

#[cfg(feature = "rust-guests")]
pub use http_scope::{Fetcher, OutboundBody, OutboundResponse, ResponseBody};
pub use instance::{GuestResourceLimits, InstanceCtx, Tenancy};
pub(crate) use native_io::IdleTimers;
pub use preparation::{PreparationIo, PreparationIoConfig};
pub use runtime::{CellRuntime, Services, WakeSink};
pub use runtime_configuration::{
    RuntimeConfiguration, RuntimeConfigurationError, RuntimeConfigurations,
};

use std::sync::atomic::Ordering;
use std::sync::Arc;

use isoplane_abi::imports::{self, ImportKind};
use isoplane_abi::result::*;
use isoplane_abi::status::TrapCode;
use isoplane_engine::{HostBindings, ImportCall};

use crate::futures::INVALID_FUTURE;

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

    pub(super) fn rt(&self) -> Option<&Arc<CellRuntime>> {
        self.ctx.rt.as_ref()
    }

    /// `None` when the dispatch's pending cap or the instance's retained-entry
    /// limit is reached. Neither stops the instance: the caller defers or
    /// refuses the one operation, and the guest is told its cap in its
    /// dispatch record so it can queue before asking.
    pub(super) fn alloc(&self, import: &'static str, rearmable: bool) -> Option<u64> {
        self.alloc_for(self.ctx.dispatch_id(), import, rearmable)
    }

    fn alloc_for(&self, dispatch: u64, import: &'static str, rearmable: bool) -> Option<u64> {
        let rt = self.rt()?;
        let limit = self.ctx.pending_futures_limit(dispatch);
        rt.futures.set_entry_limit(
            self.ctx.instance,
            crate::futures::retained_entry_limit(
                self.ctx.guest_resources.native_handles,
                self.ctx.limits.pending_futures_per_dispatch,
            ),
        );
        let allocated = if rearmable {
            rt.futures
                .alloc_rearmable(self.ctx.instance, dispatch, import, limit)
        } else {
            rt.futures.alloc(self.ctx.instance, dispatch, import, limit)
        };
        match allocated {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::debug!(
                    tenant = %self.ctx.tenant, instance = %self.ctx.instance, dispatch, import,
                    limit = e.limit, outstanding = e.outstanding, scope = ?e.scope,
                    "pending-futures cap reached"
                );
                None
            }
        }
    }

    /// An import's future, or [`INVALID_FUTURE`] when the cap refuses it: the
    /// guest sees that one operation fail and nothing else.
    fn alloc_or_refuse(&self, import: &'static str) -> u64 {
        if let Some(id) = self.alloc(import, false) {
            return id;
        }
        #[cfg(any(test, feature = "test-util"))]
        if let Some(rt) = self.rt() {
            rt.metrics
                .pending_cap_rejections
                .fetch_add(1, Ordering::Relaxed);
        }
        tracing::warn!(
            tenant = %self.ctx.tenant, instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(), import,
            "pending-futures cap refused an operation"
        );
        INVALID_FUTURE
    }

    /// A future owned by `dispatch`, already complete, so its wake gives that
    /// dispatch a slice once the current one ends; 0 if `dispatch` is not in
    /// progress on this instance.
    fn dispatch_ready(&self, dispatch: u64) -> u64 {
        if dispatch == crate::futures::INSTANCE_WORK {
            self.ctx.background.requested.store(true, Ordering::Release);
            if let Some(rt) = self.rt() {
                rt.wake.signal(self.ctx.instance);
            }
            return 0;
        }
        if dispatch == 0 || self.ctx.dispatch(dispatch).is_none() {
            return 0;
        }
        let Some(rt) = self.rt() else {
            return INVALID_FUTURE;
        };
        let id = rt.futures.alloc_slice_request(self.ctx.instance, dispatch);
        self.settle_now(id, ResultRecord::Ok(Vec::new()))
    }

    pub(super) fn settle(rt: &Arc<CellRuntime>, id: u64, res: ResultRecord) {
        rt.settle_future(id, res);
    }

    /// For policy refusals: the guest still learns through the future.
    pub(super) fn settle_now(&self, id: u64, res: ResultRecord) -> u64 {
        if let Some(rt) = self.rt() {
            Self::settle(rt, id, res);
        }
        id
    }

    #[cfg(feature = "rust-guests")]
    pub(super) fn decode<T: serde::de::DeserializeOwned>(&self, payload: &[u8]) -> Option<T> {
        match postcard::from_bytes(payload) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(tenant = %self.ctx.tenant, error = %e, "malformed import payload");
                self.ctx.latch_trap(TrapCode::Protocol);
                None
            }
        }
    }
}

pub(super) fn ok_bytes<T: serde::Serialize>(v: &T) -> ResultRecord {
    match ResultRecord::ok(v) {
        Ok(r) => r,
        Err(e) => ResultRecord::Err(FutureError::Io(format!("encode: {e}"))),
    }
}

pub(super) fn from_typed<T: serde::Serialize>(r: Result<T, FutureError>) -> ResultRecord {
    match r {
        Ok(v) => ok_bytes(&v),
        Err(e) => ResultRecord::Err(e),
    }
}

impl HostBindings for CellBindings {
    fn restore_prepared_resources(
        &mut self,
        prepared: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), isoplane_engine::bindings::BindingError> {
        let now = native_io::Context::runtime(self)
            .map_or_else(tokio::time::Instant::now, |runtime| {
                native_io::reactor_now(&runtime)
            });
        self.ctx.native_io.lock().restore_prepared(prepared, now).map_err(|error| {
            tracing::warn!(tenant = %self.ctx.tenant, instance = %self.ctx.instance, %error, "prepared native resources refused");
            isoplane_engine::bindings::BindingError::InvalidRequest("invalid prepared native resources")
        })
    }

    fn memory_limit_bytes(&self) -> Option<u64> {
        Some(self.ctx.limits.instance_memory_max)
    }

    fn native_io(
        &mut self,
        request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        let starts_work = match &request {
            isoplane_abi::native_io::Request::Timer(duration) => duration.0 != 0,
            isoplane_abi::native_io::Request::Ready(_)
            | isoplane_abi::native_io::Request::ErrorMessage(_)
            | isoplane_abi::native_io::Request::ErrorCode(_)
            | isoplane_abi::native_io::Request::SocketAddress { .. }
            | isoplane_abi::native_io::Request::NoDelay { .. }
            | isoplane_abi::native_io::Request::KeepAlive { .. }
            | isoplane_abi::native_io::Request::BufferSize { .. }
            | isoplane_abi::native_io::Request::Shutdown { .. }
            | isoplane_abi::native_io::Request::Poll(_)
            | isoplane_abi::native_io::Request::Drop { .. } => false,
            isoplane_abi::native_io::Request::Network
            | isoplane_abi::native_io::Request::Resolve { .. }
            | isoplane_abi::native_io::Request::NextAddress(_)
            | isoplane_abi::native_io::Request::CreateTcp(_)
            | isoplane_abi::native_io::Request::Connect { .. }
            | isoplane_abi::native_io::Request::FinishConnect(_)
            | isoplane_abi::native_io::Request::Read { .. }
            | isoplane_abi::native_io::Request::TrySend { .. }
            | isoplane_abi::native_io::Request::Subscribe(_) => true,
        };
        if self.ctx.is_cancelled() && starts_work {
            return Err(isoplane_abi::native_io::Error::Denied(
                "dispatch is cancelled".into(),
            ));
        }
        self.ctx.native_io.lock().call(request, self)
    }

    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        let def = match imports::lookup(call.import) {
            Some(d) => d,
            None => {
                self.ctx.latch_trap(TrapCode::CapabilityViolation);
                return INVALID_FUTURE;
            }
        };
        match def.kind {
            ImportKind::Future => {}
            ImportKind::Sync => {
                self.ctx.latch_trap(TrapCode::Protocol);
                return INVALID_FUTURE;
            }
        };
        if def.name == "dispatch_ready" {
            return self.dispatch_ready(call.scalars[0]);
        }
        let id = self.alloc_or_refuse(def.name);
        if id == INVALID_FUTURE {
            return INVALID_FUTURE;
        }

        // In the grace window every import answers `Cancelled` through its
        // future, so an over-budget dispatch cannot spend the window issuing
        // work the host will not do.
        if self.ctx.is_cancelled() {
            return self.settle_now(id, ResultRecord::Err(FutureError::Cancelled));
        }

        let Some(rt) = self.rt().cloned() else {
            return id;
        };

        match def.name {
            #[cfg(feature = "rust-guests")]
            native_io::DEADLINE_IMPORT => {
                // The count includes the future allocated above.
                let timers = self.ctx.native_io.lock().timers(self);
                if timers > isoplane_abi::native_io::MAX_TIMERS {
                    return self.settle_now(
                        id,
                        ResultRecord::Err(FutureError::Denied(format!(
                            "an instance holds at most {} timers",
                            isoplane_abi::native_io::MAX_TIMERS
                        ))),
                    );
                }
                // SECURITY: side-channel mitigation. Counted from the slice's
                // start, as a native timer is; see `native_io::Timer`.
                let deadline = self
                    .ctx
                    .slice_start()
                    .unwrap_or_else(tokio::time::Instant::now)
                    + std::time::Duration::from_millis(call.scalars[0]);
                let rt2 = rt.clone();
                let task = self.ctx.spawn(&rt.reactor, async move {
                    tokio::time::sleep_until(deadline).await;
                    CellBindings::settle(&rt2, id, ResultRecord::Ok(Vec::new()));
                });
                rt.futures.attach_task(id, task.abort_handle());
                id
            }

            #[cfg(feature = "rust-guests")]
            "http_fetch" => self.http_fetch(rt, id, call.payload),
            #[cfg(feature = "rust-guests")]
            "http_body_read" => self.http_body_read(rt, id, call.scalars),
            "req_body_read" => self.req_body_read(rt, id, call.scalars),
            "resp_write" => self.resp_write(rt, id, call.payload, call.scalars),

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
            "log_emit" => self.log_emit(call.payload, call.scalars),
            #[cfg(feature = "rust-guests")]
            "trace_span_begin" => self.trace_span_begin(call.payload),
            #[cfg(feature = "rust-guests")]
            "trace_span_end" => self.trace_span_end(call.scalars),

            "clock_now_coarse" => ctx.slice_clock_ms() as i64,

            "future_cancel" => {
                if let Some(rt) = &ctx.rt {
                    rt.futures
                        .cancel_owned(ctx.instance, call.scalars[0].into());
                }
                0
            }
            "req_body_can_have_body" => self.req_body_can_have_body(),
            "resp_new" => self.resp_new(call.payload),
            "resp_finish" => self.resp_finish(call.scalars),

            other => {
                tracing::error!(import = other, "sync import with no route");
                ctx.latch_trap(TrapCode::CapabilityViolation);
                0
            }
        }
    }

    /// The host writes into a host-side buffer and the engine copies it in, so
    /// no host pointer enters guest memory.
    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        out: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        use isoplane_engine::bindings::BindingError;
        match call.import {
            "entropy_fill" => {
                if out.len() > imports::MAX_OUT_BUFFER_BYTES {
                    return Err(BindingError::InvalidRequest(
                        "entropy draw exceeds the output-buffer cap",
                    ));
                }
                crate::entropy::fill(out)
                    .map_err(|error| BindingError::Entropy(std::io::Error::other(error)))?;

                Ok(out.len() as i64)
            }
            "runtime_config_get" => {
                let configuration = self
                    .rt()
                    .map(|rt| {
                        rt.runtime_configuration
                            .get(&self.ctx.tenant, &self.ctx.module)
                    })
                    .unwrap_or_default();
                Ok(configuration.fill(out))
            }
            #[cfg(feature = "rust-guests")]
            "init_hash_seed" | "init_config_get" => {
                self.ctx.latch_trap(TrapCode::CapabilityViolation);
                Err(BindingError::Unsupported(call.import))
            }
            other => {
                tracing::error!(import = other, "fill import with no route");
                self.ctx.latch_trap(TrapCode::Protocol);
                Err(BindingError::Unsupported(other))
            }
        }
    }

    /// Snapshots native timer time and cancellation as the engine enters the guest.
    fn begin_slice(&mut self) {
        let now = match self.rt() {
            Some(runtime) => {
                let _reactor = runtime.reactor.enter();
                tokio::time::Instant::now()
            }
            None => tokio::time::Instant::now(),
        };
        self.ctx.begin_guest_slice(now);
    }

    /// Called mid-slice from the epoch callback, so it only requests: the
    /// guest sees the cancellation from its next slice (see `cancellation`).
    fn enter_grace(&mut self) {
        if self.ctx.dispatch_id() == crate::futures::INSTANCE_WORK {
            return;
        }
        if self.ctx.current().request_cancellation() {
            tracing::debug!(
                tenant = %self.ctx.tenant, instance = %self.ctx.instance,
                dispatch = self.ctx.dispatch_id(),
                "dispatch entered its grace window: cancellation requested"
            );
        }
    }

    /// Completing the cancellation future is what wakes a parked guest to
    /// unwind. Done between slices, so the future's readiness is never a
    /// mid-slice signal either.
    fn end_slice(&mut self) {
        if let Some(cancel) = self.ctx.take_undelivered_cancellation() {
            self.settle_now(cancel, ResultRecord::Err(FutureError::Cancelled));
        }
    }

    fn slice_clock_ms(&self) -> u64 {
        self.ctx.slice_clock_ms()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstanceId;

    use isoplane_abi::imports::IMPORTS;

    pub(super) fn ctx() -> Arc<InstanceCtx> {
        Arc::new(crate::fakes::instance_ctx(
            InstanceId(1),
            isoplane_abi::config::Limits::default(),
        ))
    }

    pub(super) fn call<'a>(
        import: &'static str,
        payload: &'a [u8],
        scalars: [u64; 3],
    ) -> ImportCall<'a> {
        ImportCall {
            import,
            payload,
            scalars,
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
            let result = b.call_fill(call(def.name, &[], [0; 3]), &mut out);
            let _ = c.take_trap();
            assert!(
                result.is_ok() || matches!(def.name, "init_hash_seed" | "init_config_get"),
                "{} has an out buffer but call_fill refuses it",
                def.name
            );
        }
    }

    #[test]
    fn the_coarse_clock_reads_the_host_selected_slice_time() {
        let c = ctx();
        c.begin_dispatch(1_000, 1);
        let mut b = CellBindings::new(c.clone());
        assert_eq!(
            b.call_sync(call("clock_now_coarse", &[], [0; 3])),
            1_000,
            "the guest reads the selected slice clock"
        );
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
            let n = b
                .call_fill(call("entropy_fill", &[], [0; 3]), &mut buf)
                .expect("host entropy");
            assert_eq!(n, 32, "the whole buffer is filled");
            assert!(buf.iter().any(|x| *x != 0), "entropy returned zeros");
            assert!(
                seen.insert(buf.to_vec()),
                "entropy repeated: it was cached somewhere"
            );
        }
    }

    #[test]
    fn an_out_buffer_larger_than_the_cap_is_refused() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut big = vec![0u8; imports::MAX_OUT_BUFFER_BYTES + 1];
        assert!(
            b.call_fill(call("entropy_fill", &[], [0; 3]), &mut big)
                .is_err(),
            "a fill past the out-buffer cap is refused"
        );
        assert!(big.iter().all(|x| *x == 0), "and nothing was written");
    }

    #[test]
    fn entropy_through_a_path_with_no_out_buffer_is_denied() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        assert_eq!(
            b.call_sync(call("entropy_fill", &[], [32, 0, 0])),
            0,
            "the sync path has nowhere to put the bytes"
        );
        assert_eq!(
            c.take_trap(),
            Some(TrapCode::CapabilityViolation),
            "and refuses the import as unrouted"
        );
    }

    #[test]
    fn independent_contexts_receive_distinct_live_entropy_draws() {
        let a = Arc::new(crate::fakes::instance_ctx(
            InstanceId(1),
            isoplane_abi::config::Limits::default(),
        ));
        let z = Arc::new(crate::fakes::instance_ctx(
            InstanceId(2),
            isoplane_abi::config::Limits::default(),
        ));
        let mut ba = CellBindings::new(a);
        let mut bz = CellBindings::new(z);
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        ba.call_fill(call("entropy_fill", &[], [0; 3]), &mut x)
            .expect("first draw");
        bz.call_fill(call("entropy_fill", &[], [0; 3]), &mut y)
            .expect("second draw");
        assert_ne!(x, y, "independent host draws returned the same bytes");
    }

    #[test]
    fn the_deploy_hash_seed_is_not_reachable_at_runtime() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        let mut seed = [0u8; 16];
        assert!(
            b.call_fill(call("init_hash_seed", &[], [0; 3]), &mut seed)
                .is_err(),
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
    fn the_two_call_paths_do_not_accept_each_others_imports() {
        let c = ctx();
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("http_fetch", &[], [0; 3]));
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
}
