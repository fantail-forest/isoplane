//! The `log_emit` and `trace_span_*` import arms.

use std::sync::atomic::Ordering;

use crate::meters::OccupancyInterval;

use super::CellBindings;

impl CellBindings {
    /// Log occupancy is already on the dispatch's meter; the interval makes the
    /// share visible and the byte cap bounds volume.
    pub(super) fn log_emit(&self, payload: &[u8], scalars: [u64; 3]) -> i64 {
        let interval = OccupancyInterval::start();
        let Ok(level @ 0..=4) = u8::try_from(scalars[0]) else {
            self.ctx
                .latch_trap(isoplane_abi::status::TrapCode::Protocol);
            return 0;
        };
        let n = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let previous = self
            .ctx
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
        self.ctx.meters.record_log_occupancy(interval.elapsed_ns());
        0
    }

    pub(super) fn trace_span_begin(&self, payload: &[u8]) -> i64 {
        let name = String::from_utf8_lossy(payload).to_string();
        let id = self.ctx.next_span.fetch_add(1, Ordering::Relaxed) as i32;
        self.ctx
            .spans
            .lock()
            .insert(id, (name, self.ctx.slice_clock_ms()));
        id as i64
    }

    pub(super) fn trace_span_end(&self, scalars: [u64; 3]) -> i64 {
        let id = scalars[0] as i32;
        if let Some((name, at)) = self.ctx.spans.lock().remove(&id) {
            tracing::debug!(tenant = %self.ctx.tenant, span = %name, began_ms = at, "span end");
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{call, ctx};
    use super::*;
    use isoplane_engine::HostBindings;
    use std::sync::Arc;

    #[test]
    fn logging_records_its_occupancy_share_without_billing_it_twice() {
        let output = TestLogCapture::default();
        let subscriber = output.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&subscriber);
        let context = ctx();
        context.begin_dispatch(1, 1);
        let mut bindings = CellBindings::new(context.clone());
        crate::occupancy_clock::with_readings([100, 350], || {
            bindings.call_sync(call("log_emit", b"a", [2, 0, 0]));
        });
        assert_eq!(context.meters.log_occupancy_ns(), 250);
        assert_eq!(
            context.meters.occupancy_ns(),
            0,
            "the enclosing slice owns accounting"
        );
        assert_eq!(output.records().len(), 1);
    }

    #[test]
    fn the_largest_configured_log_budget_cannot_wrap() {
        let output = TestLogCapture::default();
        let subscriber = output.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&subscriber);
        let mut context = ctx();
        Arc::get_mut(&mut context)
            .expect("new context has no other owner")
            .configure_resources(super::super::GuestResourceLimits {
                log_bytes_per_dispatch: u32::MAX,
                ..Default::default()
            });
        context.log_bytes.store(u32::MAX - 1, Ordering::Relaxed);
        let mut bindings = CellBindings::new(context.clone());
        bindings.call_sync(call("log_emit", b"a", [2, 0, 0]));
        bindings.call_sync(call("log_emit", b"b", [2, 0, 0]));
        assert_eq!(
            output.records().len(),
            1,
            "the largest configured allowance must reject bytes beyond its boundary"
        );
    }

    #[test]
    fn configured_log_byte_limits_are_enforced() {
        for (bytes, expected) in [(3, 1), (100, 2), (0, 0)] {
            let output = TestLogCapture::default();
            let subscriber = output.dispatch();
            let _subscriber = tracing::dispatcher::set_default(&subscriber);
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
                output.records().len(),
                expected,
                "configured log allowances must be enforced"
            );
        }
    }

    #[test]
    fn log_levels_outside_the_canonical_range_latch_a_protocol_trap() {
        for level in [5, 256, u64::MAX] {
            let output = TestLogCapture::default();
            let subscriber = output.dispatch();
            let _subscriber = tracing::dispatcher::set_default(&subscriber);
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
                output.records().is_empty(),
                "malformed log calls cannot publish customer records"
            );
        }
    }

    #[test]
    fn log_volume_accepts_the_boundary_and_cannot_wrap_back_into_a_fresh_allowance() {
        let output = TestLogCapture::default();
        let subscriber = output.dispatch();
        let _subscriber = tracing::dispatcher::set_default(&subscriber);
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
            output.records().len(),
            1,
            "the exact log-byte boundary is accepted and subsequent bytes are refused"
        );
        context.log_bytes.store(u32::MAX, Ordering::Relaxed);
        bindings.call_sync(call("log_emit", b"cannot wrap", [2, 0, 0]));
        assert_eq!(
            output.records().len(),
            1,
            "overflow cannot reset a dispatch's exhausted log allowance"
        );
        context.begin_dispatch(2, 2);
        bindings.call_sync(call("log_emit", b"new dispatch", [2, 0, 0]));
        assert_eq!(
            output.records().len(),
            2,
            "a new dispatch obtains its own log allowance"
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
