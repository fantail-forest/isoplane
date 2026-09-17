use parking_lot::RwLock;
use std::ffi::c_void;
use std::sync::Arc;

use tracing::{field::Visit, Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

#[derive(Clone)]
pub(crate) struct Events(Arc<RwLock<Option<Sink>>>);

struct Sink {
    callback: extern "C" fn(*mut c_void, *const u8, usize),
    context: usize,
}

impl Events {
    pub fn new(
        callback: extern "C" fn(*mut c_void, *const u8, usize),
        context: *mut c_void,
    ) -> Self {
        Self(Arc::new(RwLock::new(Some(Sink {
            callback,
            context: context as usize,
        }))))
    }

    pub fn emit(&self, event: serde_json::Value) {
        let bytes = event.to_string();
        if let Some(sink) = self.0.read().as_ref() {
            (sink.callback)(sink.context as *mut c_void, bytes.as_ptr(), bytes.len());
        }
    }

    pub fn lifetime(&self) -> EventLifetime {
        EventLifetime {
            events: self.clone(),
        }
    }

    pub fn subscriber(
        &self,
        filter: &str,
    ) -> Result<tracing::Dispatch, tracing_subscriber::filter::ParseError> {
        let filter = tracing_subscriber::EnvFilter::try_new(filter)?;
        Ok(tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(filter)
                .with(self.clone()),
        ))
    }
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

impl<S: Subscriber> tracing_subscriber::Layer<S> for Events {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields(serde_json::Map::new());
        event.record(&mut fields);
        self.emit(serde_json::json!({
            "event": "diagnostic", "target": event.metadata().target(),
            "level": event.metadata().level().as_str(), "fields": fields.0,
        }));
    }
}

struct Fields(serde_json::Map<String, serde_json::Value>);

impl Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    extern "C" fn record(context: *mut c_void, _: *const u8, _: usize) {
        // SAFETY: this test keeps its counter alive until every event source is dropped.
        let counter = unsafe { &*context.cast::<AtomicUsize>() };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn retained_event_sources_cannot_call_a_released_managed_context() {
        let counter = AtomicUsize::new(0);
        let events = Events::new(record, (&counter as *const AtomicUsize).cast_mut().cast());
        let lifetime = events.lifetime();
        events.emit(serde_json::json!({"event":"ready"}));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "a live session must deliver its event"
        );
        drop(lifetime);
        events.emit(serde_json::json!({"event":"diagnostic"}));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "retained subscribers must not enter a released managed context"
        );
    }
}
