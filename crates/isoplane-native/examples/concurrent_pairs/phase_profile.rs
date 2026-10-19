use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use nix::sys::resource::{getrusage, Usage, UsageWho};
use nix::sys::time::TimeValLike;
use nix::time::{clock_gettime, ClockId};
use parking_lot::Mutex;
use procfs::FromRead;
use tracing::span::{Attributes, Id};
use tracing::{field::Visit, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

#[derive(Clone)]
/// Per-thread counters at selected existing synchronous span boundaries.
pub struct Profile(Arc<Output>);

struct Output {
    writer: Mutex<BufWriter<File>>,
    error: Mutex<Option<String>>,
}

impl Profile {
    /// Opens a private JSON-lines profile output.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self(Arc::new(Output {
            writer: Mutex::new(BufWriter::new(File::create(path)?)),
            error: Mutex::new(None),
        })))
    }

    /// Flushes the profile and fails if any counter or output operation failed.
    pub fn finish(&self) -> anyhow::Result<()> {
        self.0.writer.lock().flush()?;
        if let Some(error) = self.0.error.lock().as_ref() {
            anyhow::bail!("phase profiling failed: {error}");
        }
        Ok(())
    }

    fn failed(&self, error: impl std::fmt::Display) {
        self.0.error.lock().get_or_insert_with(|| error.to_string());
    }
}

#[derive(Default)]
struct Dispatch(Option<u64>);

impl Visit for Dispatch {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "dispatch_token" {
            self.0 = Some(value);
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

struct Reading {
    monotonic_ns: i64,
    usage: Usage,
    run_delay_ns: u64,
}

impl Reading {
    fn now() -> anyhow::Result<Self> {
        let scheduler =
            procfs::process::Schedstat::from_read(File::open("/proc/thread-self/schedstat")?)?;
        Ok(Self {
            monotonic_ns: clock_gettime(ClockId::CLOCK_MONOTONIC)?.num_nanoseconds(),
            usage: getrusage(UsageWho::RUSAGE_THREAD)?,
            run_delay_ns: scheduler.run_delay,
        })
    }
}

struct Active {
    span: Id,
    phase: &'static str,
    dispatch_token: u64,
    start: Reading,
}

thread_local! {
    static ACTIVE: RefCell<Vec<Active>> = const { RefCell::new(Vec::new()) };
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Profile {
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if attributes.metadata().name() == "dispatch" {
            let mut dispatch = Dispatch::default();
            attributes.record(&mut dispatch);
            if let Some(span) = context.span(id) {
                span.extensions_mut().insert(dispatch);
            }
        }
    }

    fn on_enter(&self, id: &Id, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let phase = span.metadata().name();
        if !matches!(
            phase,
            "snapshot_reservation"
                | "activation_store"
                | "activation_instantiate"
                | "activation_layer_restore"
                | "activation_table_baseline"
                | "guest_slice"
        ) {
            return;
        }
        let Some(dispatch_token) = span
            .scope()
            .find_map(|parent| parent.extensions().get::<Dispatch>().and_then(|d| d.0))
        else {
            return;
        };
        match Reading::now() {
            Ok(start) => ACTIVE.with_borrow_mut(|active| {
                active.push(Active {
                    span: id.clone(),
                    phase,
                    dispatch_token,
                    start,
                });
            }),
            Err(error) => self.failed(error),
        }
    }

    fn on_exit(&self, id: &Id, _: Context<'_, S>) {
        let active = ACTIVE.with_borrow_mut(|active| {
            let index = active.iter().rposition(|entry| entry.span == *id)?;
            Some(active.remove(index))
        });
        let Some(active) = active else { return };
        if let Err(error) = self.write(active) {
            self.failed(error);
        }
    }
}

impl Profile {
    fn write(&self, active: Active) -> anyhow::Result<()> {
        let end = Reading::now()?;
        let start = active.start;
        let row = serde_json::json!({
            "phase": active.phase,
            "dispatch_token": active.dispatch_token,
            "tid": nix::unistd::gettid().as_raw(),
            "start_ns": start.monotonic_ns,
            "end_ns": end.monotonic_ns,
            "user_cpu_us": (end.usage.user_time() - start.usage.user_time()).num_microseconds(),
            "system_cpu_us": (end.usage.system_time() - start.usage.system_time()).num_microseconds(),
            "minor_faults": end.usage.minor_page_faults() - start.usage.minor_page_faults(),
            "major_faults": end.usage.major_page_faults() - start.usage.major_page_faults(),
            "voluntary_switches": end.usage.voluntary_context_switches() - start.usage.voluntary_context_switches(),
            "involuntary_switches": end.usage.involuntary_context_switches() - start.usage.involuntary_context_switches(),
            "run_delay_ns": end.run_delay_ns - start.run_delay_ns,
        });
        let mut bytes = serde_json::to_vec(&row)?;
        bytes.push(b'\n');
        self.0.writer.lock().write_all(&bytes)?;
        Ok(())
    }
}
