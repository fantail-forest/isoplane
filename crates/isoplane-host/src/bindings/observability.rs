use std::sync::atomic::Ordering;

use super::CellBindings;

/// The tracing target of the record each guest log line becomes.
pub const GUEST_LOG_TARGET: &str = module_path!();

impl CellBindings {
    /// Log occupancy is already on the dispatch's meter; the byte cap bounds
    /// volume.
    pub(super) fn log_emit(&self, payload: &[u8], scalars: [u64; 3]) -> i64 {
        let Ok(level @ 0..=4) = u8::try_from(scalars[0]) else {
            self.ctx
                .latch_trap(isoplane_abi::status::TrapCode::Protocol);
            return 0;
        };
        let n = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let previous = self
            .ctx
            .current()
            .log_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                Some(used.saturating_add(n))
            })
            .expect("the update closure always returns Some");
        if previous
            .checked_add(n)
            .is_none_or(|used| used > self.ctx.guest_resources.log_bytes_per_dispatch)
        {
            return 0;
        }
        let fields = String::from_utf8_lossy(payload).to_string();
        tracing::info!(
            target: GUEST_LOG_TARGET,
            tenant = %self.ctx.tenant,
            app = %self.ctx.module.app,
            version = %self.ctx.module.version,
            at_ms = self.ctx.slice_clock_ms(),
            module = %self.ctx.module,
            instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(),
            level,
            %fields,
            "guest log"
        );

        0
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{call, ctx};
    use super::*;
    use isoplane_engine::HostBindings;
    use isoplane_harness::logging::LogCapture;
    use std::sync::Arc;

    #[test]
    fn the_largest_configured_log_budget_cannot_wrap() {
        let capture = LogCapture::default();
        let dispatch = capture.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&dispatch);
        let mut context = ctx();
        Arc::get_mut(&mut context)
            .expect("new context has no other owner")
            .configure_resources(super::super::GuestResourceLimits {
                log_bytes_per_dispatch: u32::MAX,
                ..Default::default()
            });
        context
            .current()
            .log_bytes
            .store(u32::MAX - 1, Ordering::Relaxed);
        let mut bindings = CellBindings::new(context.clone());
        bindings.call_sync(call("log_emit", b"a", [2, 0, 0]));
        bindings.call_sync(call("log_emit", b"b", [2, 0, 0]));
        assert_eq!(
            capture.records().len(),
            1,
            "the largest configured allowance must reject bytes beyond its boundary"
        );
    }

    #[test]
    fn configured_log_limits_bound_emitted_bytes() {
        for (bytes, expected) in [(3, 1), (100, 2), (0, 0)] {
            let capture = LogCapture::default();
            let dispatch = capture.dispatch();
            let _subscriber = tracing::dispatcher::set_default(&dispatch);
            let mut context = ctx();
            Arc::get_mut(&mut context)
                .expect("new context has no other owner")
                .configure_resources(super::super::GuestResourceLimits {
                    log_bytes_per_dispatch: bytes,
                    ..Default::default()
                });
            context.begin_dispatch(1, 1);
            let mut bindings = CellBindings::new(context.clone());
            bindings.call_sync(call("log_emit", b"abc", [2, 0, 0]));
            bindings.call_sync(call("log_emit", b"d", [2, 0, 0]));
            assert_eq!(
                capture.records().len(),
                expected,
                "configured log allowances must be enforced"
            );
        }
    }

    #[test]
    fn log_levels_outside_the_canonical_range_latch_a_protocol_trap() {
        for level in [5, 256, u64::MAX] {
            let capture = LogCapture::default();
            let dispatch = capture.dispatch();
            let _subscriber = tracing::dispatcher::set_default(&dispatch);
            let context = ctx();
            context.begin_dispatch(1, 1);
            let mut bindings = CellBindings::new(context.clone());
            bindings.call_sync(call("log_emit", b"invalid severity", [level, 0, 0]));
            assert_eq!(
                context.take_trap(),
                Some(isoplane_abi::status::TrapCode::Protocol),
                "an invalid log severity cannot wrap into a supported level"
            );
            assert!(
                capture.records().is_empty(),
                "malformed log calls cannot publish customer records"
            );
        }
    }

    #[test]
    fn log_volume_accepts_the_boundary_and_cannot_wrap_back_into_a_fresh_allowance() {
        let capture = LogCapture::default();
        let dispatch = capture.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&dispatch);
        let context = ctx();
        context.begin_dispatch(1, 1);
        let mut bindings = CellBindings::new(context.clone());
        bindings.call_sync(call(
            "log_emit",
            &vec![b'x'; context.guest_resources.log_bytes_per_dispatch as usize],
            [2, 0, 0],
        ));
        bindings.call_sync(call("log_emit", b"one byte over", [2, 0, 0]));
        assert_eq!(
            capture.records().len(),
            1,
            "the exact log-byte boundary is accepted and subsequent bytes are refused"
        );
        context
            .current()
            .log_bytes
            .store(u32::MAX, Ordering::Relaxed);
        bindings.call_sync(call("log_emit", b"cannot wrap", [2, 0, 0]));
        assert_eq!(
            capture.records().len(),
            1,
            "overflow cannot reset a dispatch's exhausted log allowance"
        );
        context.begin_dispatch(2, 2);
        bindings.call_sync(call("log_emit", b"new dispatch", [2, 0, 0]));
        assert_eq!(
            capture.records().len(),
            2,
            "a new dispatch obtains its own log allowance"
        );
    }

    #[test]
    fn logs_are_structured_charged_and_bounded() {
        let capture = LogCapture::default();
        let dispatch = capture.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&dispatch);
        let c = ctx();
        c.begin_dispatch(10, 1);
        let mut b = CellBindings::new(c.clone());
        b.call_sync(call("log_emit", b"{\"msg\":\"hello\"}", [2, 0, 0]));
        assert_eq!(capture.records().len(), 1, "the line is emitted");
        assert_eq!(capture.records()[0].level, 2, "with its level");

        let big = vec![b'x'; 64 * 1024];
        for _ in 0..8 {
            b.call_sync(call("log_emit", &big, [2, 0, 0]));
        }
        assert!(
            capture.records().len() < 9,
            "the per-dispatch log byte cap did not bite"
        );
        c.begin_dispatch(11, 2);
        b.call_sync(call("log_emit", b"after", [2, 0, 0]));
        assert!(
            capture.records().iter().any(|l| l.fields == "after"),
            "a new dispatch gets a fresh allowance"
        );
    }
}
