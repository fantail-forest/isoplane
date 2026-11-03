//! The `log_emit` and `trace_span_*` import arms.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::meters::OccupancyInterval;

use super::CellBindings;

#[derive(Default)]
pub(super) struct TraceSpans {
    next: i32,
    names_bytes: usize,
    open: HashMap<i32, GuestSpan>,
}

struct GuestSpan {
    dispatch: u64,
    name: Box<str>,
    began_ms: u64,
}

impl TraceSpans {
    fn begin(
        &mut self,
        dispatch: u64,
        payload: &[u8],
        began_ms: u64,
        limits: super::GuestResourceLimits,
    ) -> Option<i32> {
        if self.open.len() >= limits.trace_spans {
            return None;
        }
        let name_bytes = payload.utf8_chunks().try_fold(0usize, |bytes, chunk| {
            bytes
                .checked_add(chunk.valid().len())?
                .checked_add(if chunk.invalid().is_empty() { 0 } else { 3 })
        })?;
        if name_bytes > limits.trace_name_bytes.saturating_sub(self.names_bytes) {
            return None;
        }
        let id = self.next.checked_add(1)?;
        let name = String::from_utf8_lossy(payload)
            .into_owned()
            .into_boxed_str();
        self.next = id;
        self.names_bytes += name.len();
        self.open.insert(
            id,
            GuestSpan {
                dispatch,
                name,
                began_ms,
            },
        );
        Some(id)
    }

    fn end(&mut self, id: i32) -> Option<GuestSpan> {
        let span = self.open.remove(&id)?;
        self.names_bytes -= span.name.len();
        Some(span)
    }

    pub(super) fn clear(&mut self) {
        self.open.clear();
        self.names_bytes = 0;
    }

    pub(super) fn end_dispatch(&mut self, dispatch: u64) -> usize {
        let before = self.open.len();
        self.open.retain(|_, span| {
            if span.dispatch == dispatch {
                self.names_bytes -= span.name.len();
                false
            } else {
                true
            }
        });
        before - self.open.len()
    }
}

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
        let dispatch = self.ctx.current();
        if let Some(id) = self.ctx.spans.lock().begin(
            dispatch.id(),
            payload,
            self.ctx.slice_clock_ms(),
            self.ctx.guest_resources,
        ) {
            return i64::from(id);
        }
        let dropped = dispatch
            .trace_dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(1))
            })
            .expect("the update closure always returns Some");
        if dropped == 0 {
            tracing::warn!(tenant = %self.ctx.tenant, instance = %self.ctx.instance,
                dispatch = dispatch.id(), span_limit = self.ctx.guest_resources.trace_spans,
                name_bytes_limit = self.ctx.guest_resources.trace_name_bytes, "guest trace spans truncated");
        }
        0
    }

    pub(super) fn trace_span_end(&self, scalars: [u64; 3]) -> i64 {
        let id = scalars[0] as i32;
        if let Some(span) = self.ctx.spans.lock().end(id) {
            tracing::debug!(tenant = %self.ctx.tenant, dispatch = span.dispatch, span = %span.name, began_ms = span.began_ms, "span end");
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
    fn discarding_an_instance_releases_trace_and_response_buffers_while_its_context_is_retained() {
        let context = ctx();
        context.begin_dispatch(1, 1);
        let mut bindings = CellBindings::new(context.clone());
        assert_ne!(
            bindings.call_sync(call("trace_span_begin", b"retained", [0; 3])),
            0
        );
        let response = context
            .responses
            .lock()
            .create(
                1,
                isoplane_abi::dispatch::ResponseHead {
                    status: 200,
                    headers: vec![],
                    body: b"retained".to_vec(),
                    streaming: false,
                },
            )
            .expect("retain response");
        context.release_native_resources();
        assert!(context.spans.lock().open.is_empty());
        assert_eq!(context.spans.lock().names_bytes, 0);
        assert!(context.responses.lock().get(response).is_none());
    }

    #[test]
    fn trace_limits_count_lossy_utf8_bytes_and_release_capacity() {
        let mut spans = TraceSpans::default();
        let limits = super::super::GuestResourceLimits {
            trace_spans: 2,
            trace_name_bytes: 6,
            ..Default::default()
        };
        let first = spans
            .begin(1, &[0xff], 10, limits)
            .expect("replacement character fits");
        assert_eq!(spans.names_bytes, 3);
        let second = spans
            .begin(2, b"abc", 10, limits)
            .expect("exact byte boundary fits");
        assert!(
            spans.begin(1, b"", 10, limits).is_none(),
            "empty names still consume the count allowance"
        );
        assert!(spans.end(first).is_some());
        assert!(
            spans.begin(1, b"abcd", 10, limits).is_none(),
            "aggregate byte allowance applies across dispatches"
        );
        let third = spans
            .begin(1, b"abc", 10, limits)
            .expect("ended span releases its bytes");
        assert!(third > second, "stale handles cannot address a new span");
        spans.end_dispatch(1);
        assert_eq!(spans.names_bytes, 3);
        assert!(
            spans.open.contains_key(&second),
            "another dispatch retains its span"
        );
        spans.end_dispatch(2);
        assert_eq!(spans.names_bytes, 0);
    }

    #[test]
    fn disabled_trace_limits_and_exhausted_ids_refuse_without_allocating() {
        for limits in [
            super::super::GuestResourceLimits {
                trace_spans: 0,
                ..Default::default()
            },
            super::super::GuestResourceLimits {
                trace_name_bytes: 0,
                ..Default::default()
            },
        ] {
            let mut spans = TraceSpans::default();
            assert!(spans.begin(1, b"a", 1, limits).is_none());
            assert!(spans.open.is_empty());
            assert_eq!(spans.names_bytes, 0);
        }
        let mut spans = TraceSpans {
            next: i32::MAX,
            ..Default::default()
        };
        assert!(spans.begin(1, b"a", 1, Default::default()).is_none());
        assert!(spans.open.is_empty());
    }

    #[test]
    fn ending_a_dispatch_releases_its_unfinished_spans_and_resets_drop_accounting() {
        let mut context = ctx();
        Arc::get_mut(&mut context)
            .expect("new context")
            .configure_resources(super::super::GuestResourceLimits {
                trace_spans: 1,
                trace_name_bytes: 4,
                ..Default::default()
            });
        let mut bindings = CellBindings::new(context.clone());
        for dispatch in 1..=8 {
            context.begin_dispatch(dispatch, dispatch);
            assert_ne!(
                bindings.call_sync(call("trace_span_begin", b"name", [0; 3])),
                0
            );
            assert_eq!(
                bindings.call_sync(call("trace_span_begin", b"name", [0; 3])),
                0
            );
            assert_eq!(context.current().trace_dropped.load(Ordering::Relaxed), 1);
            assert_eq!(
                context.take_trap(),
                None,
                "diagnostic truncation cannot kill the application"
            );
            context.end_dispatch(dispatch);
            assert!(context.spans.lock().open.is_empty());
            assert_eq!(context.spans.lock().names_bytes, 0);
        }
    }

    #[test]
    fn logging_records_its_occupancy_share_without_counting_it_twice() {
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
            "the enclosing slice records occupied time"
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
        context
            .current()
            .log_bytes
            .store(u32::MAX - 1, Ordering::Relaxed);
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
        context
            .current()
            .log_bytes
            .store(u32::MAX, Ordering::Relaxed);
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
