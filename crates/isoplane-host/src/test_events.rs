use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{filter, Layer, Registry};

/// An event attributed to an application, as the test process's subscriber
/// recorded it.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub(crate) target: &'static str,
    pub(crate) fields: Map<String, Value>,
}

static RECORDED: Mutex<Vec<Recorded>> = Mutex::new(Vec::new());
static ARRIVED: Condvar = Condvar::new();

/// Makes the recording subscriber the global default, the one subscriber of
/// the test process.
///
/// `Cell::build` calls this first: a cell copies the default it is built
/// under into its threads and dispatches, and a copy of no subscriber taken
/// before this would let the first of them to reach a callsite disable it for
/// every thread.
pub(crate) fn install() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let recorder = Recorder.with_filter(filter::filter_fn(|metadata| {
            metadata.is_event() && metadata.fields().field("app").is_some()
        }));
        tracing::subscriber::set_global_default(Registry::default().with(recorder))
            .expect("nothing else in the test process sets a global subscriber");
    });
}

/// The events of an application whose name no other test uses.
pub(crate) struct Events {
    app: String,
}

impl Events {
    /// Installs the recording subscriber and names a fresh application after
    /// `name`.
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        install();
        Self {
            app: format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
        }
    }

    /// The name to register the test's module under.
    pub(crate) fn app(&self) -> &str {
        &self.app
    }

    /// This application's events so far.
    pub(crate) fn recorded(&self) -> Vec<Recorded> {
        self.select(&RECORDED.lock())
    }

    /// This application's dispatch completions so far.
    pub(crate) fn records(&self) -> Vec<Map<String, Value>> {
        completions(self.select(&RECORDED.lock()))
    }

    /// This application's dispatch completions once there are at least
    /// `count`; fails the test after ten seconds.
    pub(crate) fn completed(&self, count: usize) -> Vec<Map<String, Value>> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut recorded = RECORDED.lock();
        loop {
            let completions = completions(self.select(&recorded));
            if completions.len() >= count {
                return completions;
            }
            assert!(
                !ARRIVED.wait_until(&mut recorded, deadline).timed_out(),
                "dispatch completion did not arrive"
            );
        }
    }

    fn select(&self, recorded: &[Recorded]) -> Vec<Recorded> {
        recorded
            .iter()
            .filter(|event| event.fields.get("app").and_then(Value::as_str) == Some(&self.app))
            .cloned()
            .collect()
    }
}

fn completions(events: Vec<Recorded>) -> Vec<Map<String, Value>> {
    events
        .into_iter()
        .filter(|event| {
            event.fields.get("event").and_then(Value::as_str) == Some("dispatch_completed")
        })
        .map(|event| event.fields)
        .collect()
}

struct Recorder;

impl<S: tracing::Subscriber> Layer<S> for Recorder {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        RECORDED.lock().push(Recorded {
            target: event.metadata().target(),
            fields: fields.0,
        });
        ARRIVED.notify_all();
    }
}

#[derive(Default)]
struct Fields(Map<String, Value>);

impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}
