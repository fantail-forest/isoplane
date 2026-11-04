//! Test-only observation of the production tracing output boundary.

use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

use isoplane_host::bindings::InstanceCtx;
use parking_lot::Mutex;
use serde::Deserialize;
use tracing_subscriber::{fmt::MakeWriter, prelude::*};

/// Formatted output written by a tracing subscriber under test.
#[derive(Clone, Default)]
pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

/// One guest event decoded from tracing's JSON formatter.
#[derive(Clone, Debug, Deserialize)]
pub struct GuestLog {
    /// Publication order in the test writer.
    #[serde(skip)]
    pub sequence: usize,
    /// Host-established tenant.
    pub tenant: String,
    /// Host-established application.
    pub app: String,
    /// Executing application version.
    pub version: String,
    /// Host-established module.
    pub module: String,
    /// Host-established instance.
    pub instance: String,
    /// Host slice clock in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// Host-established dispatch.
    pub dispatch: u64,
    /// ABI severity: trace 0 through error 4.
    pub level: u8,
    /// Untrusted guest text.
    pub fields: String,
}

impl LogCapture {
    /// All formatted bytes, decoded as UTF-8.
    pub fn output(&self) -> String {
        String::from_utf8(self.0.lock().clone()).expect("tracing emits UTF-8")
    }

    /// Decodes guest events, retaining the formatter's publication order.
    pub fn records(&self) -> Vec<GuestLog> {
        self.output()
            .lines()
            .enumerate()
            .filter_map(|(sequence, line)| {
                let event: serde_json::Value =
                    serde_json::from_str(line).expect("tracing JSON event");
                if event["target"] != "isoplane_host::bindings::observability"
                    || event["fields"]["message"] != "guest log"
                {
                    return None;
                }
                let mut record: GuestLog = serde_json::from_value(event["fields"].clone())
                    .expect("the guest logging boundary emits complete metadata");
                record.sequence = sequence;
                Some(record)
            })
            .collect()
    }

    /// An isolated JSON subscriber for a test that drives imports on its own thread.
    pub fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .with_writer(self.clone())
                .finish(),
        )
    }

    /// Installs one guest-event writer for tests whose cells run on other threads.
    pub fn global() -> &'static Self {
        static CAPTURE: OnceLock<LogCapture> = OnceLock::new();
        CAPTURE.get_or_init(|| {
            let capture = Self::default();
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(capture.clone())
                    .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                        metadata.target() == "isoplane_host::bindings::observability"
                    })),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("one test guest-log subscriber per process");
            capture
        })
    }
}

impl Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Guest diagnostics collected by a test subscriber, outside the production instance.
pub trait GuestLogs {
    /// Returns emitted events attributed to this instance.
    fn logs(&self) -> Vec<GuestLog>;
}

impl GuestLogs for InstanceCtx {
    fn logs(&self) -> Vec<GuestLog> {
        let instance = self.instance.to_string();
        LogCapture::global()
            .records()
            .into_iter()
            .filter(|record| record.instance == instance)
            .collect()
    }
}
