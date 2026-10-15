//! Routes guest capabilities through host-owned services.
//! The coarse clock is fixed for each serving slice.
//! Scalar arguments follow declaration order after the buffer pair.

mod http;
mod http_scope;
mod instance;
mod native_io;
mod observability;
mod preparation;
pub(crate) mod request_body;
mod runtime;
mod runtime_configuration;

pub use http_scope::{OutboundBody, OutboundResponse, ResponseBody};
pub use instance::{GuestResourceLimits, InstanceCtx, Tenancy};
pub use preparation::{PreparationIo, PreparationIoConfig};
pub use runtime::{CellRuntime, Fetcher, Services, WakeSink};
pub use runtime_configuration::{
    RuntimeConfiguration, RuntimeConfigurationError, RuntimeConfigurations,
};

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rand::RngCore;

use isoplane_abi::imports::{self, ImportKind};
use isoplane_abi::result::*;
use isoplane_abi::status::TrapCode;
use isoplane_engine::{HostBindings, ImportCall};

use crate::futures::INVALID_FUTURE;

/// What each import's `scalars` entries mean; the contract crate's `BufSpec`
/// is the authority on where the buffer pair sits.
pub const SCALARS: &[(&str, &[&str])] = &[
    ("socket_call", &["operation", "arguments", "count"]),
    ("poll_call", &["operation", "arguments", "count"]),
    ("log_emit", &["level"]),
    ("trace_span_end", &["span"]),
    ("clock_deadline_in", &["ms"]),
    ("resp_write", &["response_handle"]),
    ("resp_finish", &["response_handle"]),
    ("http_body_read", &["body_handle", "max_len"]),
    ("req_body_read", &["max_bytes"]),
    ("init_unit_begin", &["unit"]),
    ("init_unit_deferred", &["unit"]),
    // Answered by the engine and never reaching this vtable, but declared.
    ("unsafe_to_trap", &["on"]),
    (
        isoplane_abi::filesystem::IMPORT,
        &["operation", "arguments_ptr", "argument_count"],
    ),
];

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

    pub(super) fn rt(&self) -> Option<&Arc<CellRuntime>> {
        self.ctx.rt.as_ref()
    }

    /// A refusal latches [`TrapCode::TooManyFutures`], because `call_future`
    /// returns a bare `u64` and cannot trap directly.
    pub(super) fn alloc(
        &self,
        import: &'static str,
        rearmable: bool,
    ) -> Option<u64> {
        let rt = self.rt()?;
        let dispatch = self.ctx.dispatch_id();
        let limit = self.ctx.limits.pending_futures_per_dispatch;
        let allocated = if rearmable {
            rt.futures
                .alloc_rearmable(self.ctx.instance, dispatch, import, limit)
        } else {
            rt.futures
                .alloc(self.ctx.instance, dispatch, import, limit)
        };
        match allocated {
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
        self.ctx.native_io.lock().restore_prepared(prepared).map_err(|error| {
            tracing::warn!(tenant = %self.ctx.tenant, instance = %self.ctx.instance, %error, "prepared native resources refused");
            isoplane_engine::bindings::BindingError::InvalidRequest("invalid prepared native resources")
        })
    }

    fn reset_prepared_resources(
        &mut self,
        prepared: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), isoplane_engine::bindings::BindingError> {
        self.ctx.release_native_resources();
        self.restore_prepared_resources(prepared)
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
            ImportKind::Future => (),
            ImportKind::Sync => {
                self.ctx.latch_trap(TrapCode::Protocol);
                return INVALID_FUTURE;
            }
        };
        let Some(id) = self.alloc(def.name, false) else {
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

        match def.name {
            "clock_deadline_in" => {
                let ms = call.scalars[0];
                let rt2 = rt.clone();
                let task = self.ctx.spawn(&rt.reactor, async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    CellBindings::settle(&rt2, id, ResultRecord::Ok(Vec::new()));
                });
                rt.futures.attach_task(id, task.abort_handle());
                id
            }

            "http_fetch" => self.http_fetch(rt, id, call.payload),
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
            // At runtime every init unit has already run or been deferred.
            "init_unit_begin" => 0,
            "init_unit_deferred" => 0,

            "log_emit" => self.log_emit(call.payload, call.scalars),
            "trace_span_begin" => self.trace_span_begin(call.payload),
            "trace_span_end" => self.trace_span_end(call.scalars),

            "clock_now_coarse" => ctx.slice_clock_ms() as i64,

            // Out-buffer imports belong to `call_fill`; here there is nowhere
            // to put the bytes, and a zero-filled buffer would be a constant
            // CSPRNG.
            "entropy_fill" | "init_hash_seed" | "init_config_get" | "runtime_config_get" => {
                ctx.latch_trap(TrapCode::Protocol);
                -1
            }

            "req_body_can_have_body" => self.req_body_can_have_body(),
            "resp_new" => self.resp_new(call.payload),
            "resp_finish" => self.resp_finish(call.scalars),


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
            "runtime_config_get" => {
                let configuration = self
                    .rt()
                    .map(|rt| {
                        rt.runtime_configuration
                            .get(&self.ctx.tenant, &self.ctx.module)
                    })
                    .unwrap_or_default();
                configuration.fill(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstanceId;
    use isoplane_abi::config::{HandlerLifecycle, Limits};
    use isoplane_abi::imports::IMPORTS;

    /// Scalars an import carries, per the contract crate's `BufSpec`.
    fn scalar_count(def: &imports::ImportDef) -> usize {
        match def.buf {
            imports::BufSpec::None => def.sig.params.len(),
            imports::BufSpec::In(_) | imports::BufSpec::Out(_) => {
                def.sig.params.len().saturating_sub(2)
            }
        }
    }

    pub(super) fn ctx() -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::for_test(
            InstanceId(1),
            HandlerLifecycle::Reuse,
            Limits::default(),
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
