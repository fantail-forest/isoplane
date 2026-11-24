use parking_lot::RwLock;
use std::ffi::c_void;
use std::sync::Arc;

use tracing::{field::Visit, Event, Metadata, Subscriber};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

use crate::interop::NativeText;

/// What a local session tells its managed owner.
pub(crate) enum SessionEvent {
    /// The application serves requests at `url`.
    Ready { app: String, url: String },
    /// A reloaded application replaced the one serving at `url`.
    Reloaded { app: String, url: String },
    /// The application left its debugger and continues at `url`.
    DebuggerDetached { app: String, url: String },
    /// The application that will continue without the debugger is prepared.
    DebuggerDetachPrepared,
    /// The application that would continue without the debugger could not be prepared.
    DebuggerDetachFailed { detail: String },
    /// A reload could not be prepared; the previous application keeps serving.
    ReloadFailed { detail: String },
    /// A line the application logged.
    ApplicationLog { level: u64, fields: String },
    /// A host log record the session's filter admitted, as one line.
    Diagnostic { text: String },
}

/// Which event a `NativeEvent` is, and so which member of its details it carries.
#[repr(C)]
pub enum NativeEventKind {
    Ready,
    Reloaded,
    DebuggerDetached,
    DebuggerDetachPrepared,
    DebuggerDetachFailed,
    ReloadFailed,
    ApplicationLog,
    Diagnostic,
}

/// An event of a local session, lent to the managed owner for the callback.
#[repr(C)]
pub struct NativeEvent {
    pub kind: NativeEventKind,
    pub details: NativeEventDetails,
}

/// What an event says, by its kind. `DebuggerDetachPrepared` says nothing.
#[repr(C)]
pub union NativeEventDetails {
    /// For `Ready`, `Reloaded` and `DebuggerDetached`.
    pub serving: NativeServing,
    /// For `DebuggerDetachFailed` and `ReloadFailed`, why; for `Diagnostic`, the record.
    pub text: NativeText,
    /// For `ApplicationLog`.
    pub application_log: NativeApplicationLog,
}

/// The application and the address it serves at.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeServing {
    pub app: NativeText,
    pub url: NativeText,
}

/// A line the application logged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NativeApplicationLog {
    /// The severity the application gave it, from 0 for trace to 4 for error.
    pub level: u64,
    /// The text the application logged.
    pub fields: NativeText,
}

impl SessionEvent {
    /// The event as its owner receives it, borrowing this one's text.
    fn native(&self) -> NativeEvent {
        let serving = |app: &str, url: &str| NativeEventDetails {
            serving: NativeServing {
                app: NativeText::borrowed(app),
                url: NativeText::borrowed(url),
            },
        };
        let text = |text: &str| NativeEventDetails {
            text: NativeText::borrowed(text),
        };
        let (kind, details) = match self {
            Self::Ready { app, url } => (NativeEventKind::Ready, serving(app, url)),
            Self::Reloaded { app, url } => (NativeEventKind::Reloaded, serving(app, url)),
            Self::DebuggerDetached { app, url } => {
                (NativeEventKind::DebuggerDetached, serving(app, url))
            }
            Self::DebuggerDetachPrepared => (NativeEventKind::DebuggerDetachPrepared, text("")),
            Self::DebuggerDetachFailed { detail } => {
                (NativeEventKind::DebuggerDetachFailed, text(detail))
            }
            Self::ReloadFailed { detail } => (NativeEventKind::ReloadFailed, text(detail)),
            Self::ApplicationLog { level, fields } => (
                NativeEventKind::ApplicationLog,
                NativeEventDetails {
                    application_log: NativeApplicationLog {
                        level: *level,
                        fields: NativeText::borrowed(fields),
                    },
                },
            ),
            Self::Diagnostic { text: record } => (NativeEventKind::Diagnostic, text(record)),
        };
        NativeEvent { kind, details }
    }
}

#[derive(Clone)]
pub(crate) struct Events(Arc<RwLock<Option<Sink>>>);

struct Sink {
    callback: extern "C" fn(*mut c_void, *const NativeEvent),
    context: usize,
}

impl Events {
    pub fn new(
        callback: extern "C" fn(*mut c_void, *const NativeEvent),
        context: *mut c_void,
    ) -> Self {
        Self(Arc::new(RwLock::new(Some(Sink {
            callback,
            context: context as usize,
        }))))
    }

    pub fn emit(&self, event: SessionEvent) {
        let native = event.native();
        if let Some(sink) = self.0.read().as_ref() {
            (sink.callback)(sink.context as *mut c_void, &native);
        }
    }

    pub fn lifetime(&self) -> EventLifetime {
        EventLifetime {
            events: self.clone(),
        }
    }

    /// Delivers the records `filter` admits: the application's log lines as
    /// application logs, and every other record as a diagnostic line.
    pub fn subscriber(
        &self,
        filter: &str,
    ) -> Result<tracing::Dispatch, tracing_subscriber::filter::ParseError> {
        let filter = tracing_subscriber::EnvFilter::try_new(filter)?;
        let diagnostics = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(self.clone())
            .with_filter(filter_fn(|metadata| !application_log(metadata)));
        Ok(tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(filter)
                .with(ApplicationLogs(self.clone()).with_filter(filter_fn(application_log)))
                .with(diagnostics),
        ))
    }
}

fn application_log(metadata: &Metadata<'_>) -> bool {
    metadata.target() == isoplane_host::bindings::GUEST_LOG_TARGET
}

pub(crate) struct EventLifetime {
    events: Events,
}

impl Drop for EventLifetime {
    fn drop(&mut self) {
        // Taking the writer joins callbacks before the managed GC handle is released.
        self.events.0.write().take();
    }
}

struct ApplicationLogs(Events);

impl<S: Subscriber> tracing_subscriber::Layer<S> for ApplicationLogs {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut line = GuestLine::default();
        event.record(&mut line);
        self.0.emit(SessionEvent::ApplicationLog {
            level: line.level,
            fields: line.fields,
        });
    }
}

/// The members of the host's record of a guest log line.
#[derive(Default)]
struct GuestLine {
    level: u64,
    fields: String,
}

impl Visit for GuestLine {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "level" {
            self.level = value;
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "fields" {
            self.fields = format!("{value:?}");
        }
    }
}

impl<'a> MakeWriter<'a> for Events {
    type Writer = DiagnosticLine<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        DiagnosticLine {
            events: self,
            text: Vec::new(),
        }
    }
}

/// One formatted record, delivered as a diagnostic once the formatter has written it.
pub(crate) struct DiagnosticLine<'a> {
    events: &'a Events,
    text: Vec<u8>,
}

impl std::io::Write for DiagnosticLine<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.text.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for DiagnosticLine<'_> {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.text).trim().to_string();
        self.events.emit(SessionEvent::Diagnostic { text });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    extern "C" fn record(context: *mut c_void, event: *const NativeEvent) {
        // SAFETY: each test keeps its log alive until every event source is dropped,
        // and the event is lent for this call.
        let (log, event) = unsafe { (&*context.cast::<Mutex<Vec<String>>>(), &*event) };
        // SAFETY: each arm reads the member its kind selects, which borrows live text.
        let entry = unsafe {
            match event.kind {
                NativeEventKind::ApplicationLog => format!(
                    "application log {}: {}",
                    event.details.application_log.level,
                    event.details.application_log.fields.read().expect("UTF-8")
                ),
                NativeEventKind::Diagnostic => {
                    format!("diagnostic: {}", event.details.text.read().expect("UTF-8"))
                }
                _ => "other".to_string(),
            }
        };
        log.lock().push(entry);
    }

    fn events(log: &Mutex<Vec<String>>) -> Events {
        Events::new(record, (log as *const Mutex<Vec<String>>).cast_mut().cast())
    }

    #[test]
    fn a_guest_log_line_is_an_application_log_and_another_record_is_one_diagnostic_line() {
        let log = Mutex::new(Vec::new());
        let events = events(&log);
        let _lifetime = events.lifetime();
        let subscriber = events.subscriber("info").expect("a filter");
        tracing::dispatcher::with_default(&subscriber, || {
            tracing::info!(target: isoplane_host::bindings::GUEST_LOG_TARGET, level = 3u8, fields = %"{\"message\":\"hi\"}", "guest log");
            tracing::info!(target: "isoplane_host::cell", instance = 7, "activated");
        });
        assert_eq!(
            *log.lock(),
            [
                "application log 3: {\"message\":\"hi\"}",
                "diagnostic: INFO isoplane_host::cell: activated instance=7",
            ],
            "the application's line keeps its severity and text, and a host record is formatted once"
        );
    }

    #[test]
    fn retained_event_sources_cannot_call_a_released_managed_context() {
        let log = Mutex::new(Vec::new());
        let events = events(&log);
        let lifetime = events.lifetime();
        events.emit(SessionEvent::DebuggerDetachPrepared);
        assert_eq!(log.lock().len(), 1, "a live session must deliver its event");
        drop(lifetime);
        events.emit(SessionEvent::DebuggerDetachPrepared);
        assert_eq!(
            log.lock().len(),
            1,
            "retained subscribers must not enter a released managed context"
        );
    }
}
