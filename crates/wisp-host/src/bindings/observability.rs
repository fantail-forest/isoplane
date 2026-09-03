//! The `log_emit` and `trace_span_*` import arms.

use std::sync::atomic::Ordering;

use crate::meters::CpuInterval;

use super::CellBindings;

pub(super) const MAX_LOG_BYTES_PER_DISPATCH: u32 = 256 * 1024;

impl CellBindings {
    /// Log CPU is already on the dispatch's meter; the interval makes the
    /// share visible and the byte cap bounds volume.
    pub(super) fn log_emit(&self, payload: &[u8], scalars: [u64; 3]) -> i64 {
        let cpu = CpuInterval::start();
        let level = scalars[0] as u8;
        let n = payload.len() as u32;
        let used = self.ctx.log_bytes.fetch_add(n, Ordering::Relaxed) + n;
        if used > MAX_LOG_BYTES_PER_DISPATCH {
            return 0;
        }
        let fields = String::from_utf8_lossy(payload).to_string();
        tracing::info!(
            tenant = %self.ctx.tenant,
            module = %self.ctx.module,
            instance = %self.ctx.instance,
            dispatch = self.ctx.dispatch_id(),
            at_ms = self.ctx.slice_clock_ms(),
            level,
            %fields,
            "guest log"
        );
        self.ctx.meters.record_log_cpu(cpu.elapsed_ns());
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
    use wisp_engine::HostBindings;

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
