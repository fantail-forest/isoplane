//! Bounded, whole-trace admission before the maintained OTLP transport.

use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::mpsc;
use std::time::Duration;

use opentelemetry::trace::{Span as _, TraceContextExt, TraceId};
use opentelemetry::Context;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::{Span, SpanData, SpanExporter, SpanProcessor};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Per-process bounds for buffering and queuing complete traces.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TraceLimits {
    /// Maximum simultaneously tracked traces; additional traces are discarded.
    pub active_traces: NonZeroUsize,
    /// Maximum ended spans retained per trace; exceeding it discards that trace.
    pub spans_per_trace: NonZeroUsize,
    /// Maximum complete traces waiting for transport; admission is all or nothing.
    pub queued_traces: NonZeroUsize,
    /// Deadline for an explicit flush to await queued complete traces.
    pub flush_timeout_ms: NonZeroU64,
}

impl Default for TraceLimits {
    fn default() -> Self {
        Self {
            active_traces: NonZeroUsize::new(1024).expect("1024 is nonzero"),
            spans_per_trace: NonZeroUsize::new(4096).expect("4096 is nonzero"),
            queued_traces: NonZeroUsize::new(64).expect("64 is nonzero"),
            flush_timeout_ms: NonZeroU64::new(5000).expect("5000 is nonzero"),
        }
    }
}

#[derive(Debug, Default)]
struct Trace {
    active: usize,
    ended: Option<Vec<SpanData>>,
}

#[derive(Debug)]
enum Export {
    Trace(Vec<SpanData>),
    Flush(mpsc::SyncSender<OTelSdkResult>),
}

#[derive(Debug)]
struct State {
    traces: HashMap<TraceId, Trace>,
    sender: Option<mpsc::SyncSender<Export>>,
}

/// Holds spans until their complete local trace can be handed to the exporter.
#[derive(Debug)]
pub(super) struct CompleteTraces {
    state: Mutex<State>,
    finished: Mutex<Option<mpsc::Receiver<OTelSdkResult>>>,
    limits: TraceLimits,
}

impl CompleteTraces {
    pub(super) fn new(
        exporter: impl SpanExporter + 'static,
        limits: TraceLimits,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(limits.queued_traces.get());
        let (done, finished) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("isoplane-trace-export".into())
            .spawn(move || {
                let mut result = Ok(());
                while let Ok(command) = receiver.recv() {
                    match command {
                        Export::Trace(spans) => {
                            if let Err(error) = futures::executor::block_on(exporter.export(spans))
                            {
                                tracing::warn!(
                                    event = "otlp_trace_export_failed",
                                    "complete trace export failed"
                                );
                                result = Err(error);
                            }
                        }
                        Export::Flush(reply) => {
                            if reply.send(std::mem::replace(&mut result, Ok(()))).is_err() {
                                tracing::debug!("trace flush caller no longer waiting");
                            }
                        }
                    }
                }
                if let Err(error) = exporter.shutdown() {
                    result = Err(error);
                }
                if done.send(result).is_err() {
                    tracing::debug!("trace shutdown caller no longer waiting");
                }
            })?;
        Ok(Self {
            state: Mutex::new(State {
                traces: HashMap::new(),
                sender: Some(sender),
            }),
            finished: Mutex::new(Some(finished)),
            limits,
        })
    }
}

impl SpanProcessor for CompleteTraces {
    fn on_start(&self, span: &mut Span, context: &Context) {
        let mut state = self.state.lock();
        if state.sender.is_none() {
            return;
        }
        let id = span.span_context().trace_id();
        if let Some(trace) = state.traces.get_mut(&id) {
            trace.active = trace.active.saturating_add(1);
        } else if !context.span().span_context().is_valid()
            || context.span().span_context().is_remote()
        {
            if state.traces.len() < self.limits.active_traces.get() {
                state.traces.insert(
                    id,
                    Trace {
                        active: 1,
                        ended: Some(Vec::new()),
                    },
                );
            } else {
                tracing::debug!(event = "trace_dropped", reason = "active_trace_limit");
            }
        }
    }

    fn on_end(&self, span: SpanData) {
        let mut state = self.state.lock();
        let id = span.span_context.trace_id();
        let Some(trace) = state.traces.get_mut(&id) else {
            return;
        };
        trace.active -= 1;
        if let Some(ended) = &mut trace.ended {
            if ended.len() < self.limits.spans_per_trace.get() {
                ended.push(span);
            } else {
                trace.ended = None;
                tracing::debug!(event = "trace_dropped", reason = "span_limit");
            }
        }
        if trace.active != 0 {
            return;
        }
        if let Some(Trace {
            ended: Some(spans), ..
        }) = state.traces.remove(&id)
        {
            if let Some(sender) = &state.sender {
                if sender.try_send(Export::Trace(spans)).is_err() {
                    tracing::debug!(
                        event = "trace_dropped",
                        reason = "export_queue_full_or_closed"
                    );
                }
            }
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        let (reply, response) = mpsc::sync_channel(1);
        {
            let state = self.state.lock();
            let Some(sender) = &state.sender else {
                return Ok(());
            };
            sender.try_send(Export::Flush(reply)).map_err(|_| {
                OTelSdkError::InternalFailure("trace export queue full or closed".into())
            })?;
        }
        let timeout = Duration::from_millis(self.limits.flush_timeout_ms.get());
        response
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => OTelSdkError::Timeout(timeout),
                mpsc::RecvTimeoutError::Disconnected => {
                    OTelSdkError::InternalFailure("trace exporter stopped".into())
                }
            })?
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        {
            let mut state = self.state.lock();
            state.traces.clear();
            state.sender.take();
        }
        let Some(finished) = self.finished.lock().take() else {
            return Ok(());
        };
        finished
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => OTelSdkError::Timeout(timeout),
                mpsc::RecvTimeoutError::Disconnected => {
                    OTelSdkError::InternalFailure("trace exporter stopped".into())
                }
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use std::sync::Arc;

    #[derive(Clone, Debug, Default)]
    struct Collected(Arc<Mutex<Vec<Vec<SpanData>>>>);

    impl SpanExporter for Collected {
        async fn export(&self, spans: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().push(spans);
            Ok(())
        }
    }

    fn provider(exporter: impl SpanExporter + 'static, limits: TraceLimits) -> SdkTracerProvider {
        SdkTracerProvider::builder()
            .with_span_processor(CompleteTraces::new(exporter, limits).expect("export thread"))
            .build()
    }

    fn complete(tracer: &opentelemetry_sdk::trace::Tracer, name: &'static str) {
        let root = Context::new().with_span(tracer.start_with_context(name, &Context::new()));
        tracer.start_with_context("stage", &root).end();
        root.span().end();
    }

    #[test]
    fn a_flush_never_sends_an_unfinished_trace() {
        let collected = Collected::default();
        let provider = provider(collected.clone(), TraceLimits::default());
        let tracer = provider.tracer("test");
        let root = Context::new().with_span(tracer.start_with_context("request", &Context::new()));
        tracer.start_with_context("stage", &root).end();
        provider.force_flush().expect("flush barrier");
        assert!(
            collected.0.lock().is_empty(),
            "an ended child must not escape its unfinished trace"
        );
        root.span().end();
        provider.force_flush().expect("complete flush");
        let batches = collected.0.lock();
        assert_eq!(
            batches.len(),
            1,
            "one complete trace must be one export unit"
        );
        assert_eq!(
            batches[0].len(),
            2,
            "the root and child must travel together"
        );
        drop(batches);
        provider.shutdown().expect("shutdown");
    }

    #[test]
    fn shutdown_discards_unfinished_traces() {
        let collected = Collected::default();
        let provider = provider(collected.clone(), TraceLimits::default());
        let tracer = provider.tracer("test");
        let root = Context::new().with_span(tracer.start_with_context("request", &Context::new()));
        tracer.start_with_context("stage", &root).end();
        provider.shutdown().expect("shutdown");
        root.span().end();
        assert!(
            collected.0.lock().is_empty(),
            "shutdown must discard the entire unfinished trace"
        );
    }

    #[test]
    fn a_span_limit_discards_the_whole_trace_and_allows_the_next_one() {
        let collected = Collected::default();
        let provider = provider(
            collected.clone(),
            TraceLimits {
                spans_per_trace: NonZeroUsize::new(1).unwrap(),
                ..Default::default()
            },
        );
        let tracer = provider.tracer("test");
        complete(&tracer, "too-large");
        tracer.start_with_context("fits", &Context::new()).end();
        provider.shutdown().expect("shutdown");
        let batches = collected.0.lock();
        assert_eq!(
            batches.len(),
            1,
            "an oversized trace must not poison later traces"
        );
        assert_eq!(
            batches[0].len(),
            1,
            "no fragment of the oversized trace may escape"
        );
        assert_eq!(
            batches[0][0].name, "fits",
            "only the admitted whole trace may export"
        );
    }

    #[test]
    fn an_active_trace_limit_discards_additional_roots_and_their_children() {
        let collected = Collected::default();
        let provider = provider(
            collected.clone(),
            TraceLimits {
                active_traces: NonZeroUsize::new(1).unwrap(),
                ..Default::default()
            },
        );
        let tracer = provider.tracer("test");
        let held = tracer.start_with_context("admitted", &Context::new());
        complete(&tracer, "refused");
        drop(held);
        provider.shutdown().expect("shutdown");
        let batches = collected.0.lock();
        assert_eq!(
            batches.len(),
            1,
            "capacity refusal must discard the additional whole trace"
        );
        assert_eq!(
            batches[0].len(),
            1,
            "children of a refused root must not be exported independently"
        );
        assert_eq!(
            batches[0][0].name, "admitted",
            "the original trace must retain its reservation"
        );
    }

    #[derive(Debug)]
    struct PausedExporter {
        collected: Collected,
        entered: mpsc::SyncSender<()>,
        resume: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl SpanExporter for PausedExporter {
        async fn export(&self, spans: Vec<SpanData>) -> OTelSdkResult {
            if let Some(resume) = self.resume.lock().take() {
                self.entered.send(()).expect("test waits for transport");
                resume.recv().expect("test releases transport");
            }
            self.collected.0.lock().push(spans);
            Ok(())
        }
    }

    #[test]
    fn a_full_export_queue_discards_whole_traces() {
        let collected = Collected::default();
        let (entered, waiting) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let provider = provider(
            PausedExporter {
                collected: collected.clone(),
                entered,
                resume: Mutex::new(Some(resumed)),
            },
            TraceLimits {
                queued_traces: NonZeroUsize::new(1).unwrap(),
                ..Default::default()
            },
        );
        let tracer = provider.tracer("test");
        complete(&tracer, "transporting");
        waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("transport must start without hanging");
        complete(&tracer, "queued");
        complete(&tracer, "refused");
        resume.send(()).expect("release transport");
        provider.shutdown().expect("shutdown");
        let batches = collected.0.lock();
        assert_eq!(
            batches.len(),
            2,
            "only the transporting and queued traces may export"
        );
        assert!(
            batches.iter().all(|batch| batch.len() == 2),
            "queue pressure must never truncate a trace"
        );
        assert!(
            batches.iter().flatten().all(|span| span.name != "refused"),
            "the refused trace must be discarded in full"
        );
    }

    #[test]
    fn a_stalled_exporter_does_not_make_flush_wait_indefinitely() {
        let (entered, waiting) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let provider = provider(
            PausedExporter {
                collected: Collected::default(),
                entered,
                resume: Mutex::new(Some(resumed)),
            },
            TraceLimits {
                flush_timeout_ms: NonZeroU64::new(1).unwrap(),
                ..Default::default()
            },
        );
        complete(&provider.tracer("test"), "blocked-transport");
        waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("transport must reach its controlled barrier");
        let result = provider.force_flush();
        resume
            .send(())
            .expect("release transport before any assertion");
        provider.shutdown().expect("shutdown after release");
        assert!(
            result.is_err(),
            "flush must fail while transport is held beyond its configured deadline"
        );
    }

    #[test]
    fn zero_trace_limits_are_rejected() {
        for name in [
            "active_traces",
            "spans_per_trace",
            "queued_traces",
            "flush_timeout_ms",
        ] {
            assert!(
                serde_json::from_value::<TraceLimits>(serde_json::json!({name: 0})).is_err(),
                "zero {name} must be refused during configuration parsing"
            );
        }
    }
}
