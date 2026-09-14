//! Human application logs and opt-in structured runtime diagnostics.

use std::io::Write as _;

use serde::Deserialize;
use tracing::{field::Visit, Event, Metadata, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;

const GUEST_LOG_TARGET: &str = "wisp_host::bindings::observability";

pub(super) fn init(json: bool, color: bool) -> anyhow::Result<()> {
    if json {
        let filter = tracing_subscriber::EnvFilter::builder()
            .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
            .from_env()?;
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|error| {
                anyhow::anyhow!("initializing structured runtime diagnostics: {error}")
            })?;
    } else {
        tracing_subscriber::registry()
            .with(ApplicationLogLayer::new(std::io::stderr, color))
            .try_init()
            .map_err(|error| anyhow::anyhow!("initializing application logs: {error}"))?;
    }
    Ok(())
}

struct ApplicationLogLayer<W> {
    writer: W,
    color: bool,
}

impl<W> ApplicationLogLayer<W> {
    fn new(writer: W, color: bool) -> Self {
        Self { writer, color }
    }
}

impl<S, W> tracing_subscriber::Layer<S> for ApplicationLogLayer<W>
where
    S: Subscriber,
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    fn enabled(&self, metadata: &Metadata<'_>, _: Context<'_, S>) -> bool {
        metadata.target() == GUEST_LOG_TARGET
    }

    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = GuestLogFields::default();
        event.record(&mut fields);
        let Some(line) = fields.render(self.color) else {
            return;
        };
        let mut output = self.writer.make_writer();
        drop(writeln!(output, "{line}"));
    }
}

#[derive(Default)]
struct GuestLogFields {
    level: Option<u8>,
    fields: Option<String>,
}

impl GuestLogFields {
    fn render(self, color: bool) -> Option<String> {
        let level = self.level?;
        let fields = self.fields?;
        let structured = serde_json::from_str::<StructuredApplicationLog>(&fields).ok();
        if structured.is_none() && level < 2 {
            return None;
        }
        Some(render(level, &fields, structured.as_ref(), color))
    }
}

impl Visit for GuestLogFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "level" {
            self.level = u8::try_from(value).ok().filter(|level| *level <= 4);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "fields" {
            self.fields = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "fields" {
            self.fields = Some(format!("{value:?}"));
        }
    }
}

#[derive(Deserialize)]
struct StructuredApplicationLog {
    category: String,
    severity: String,
    message: String,
    exception: Option<String>,
}

fn render(
    level: u8,
    fields: &str,
    structured: Option<&StructuredApplicationLog>,
    color: bool,
) -> String {
    let style = wisp_cli::render::Style::new(color);
    let declared = structured.map(|record| record.severity.as_str());
    let label = level_label(level, declared);
    let padded = format!("{label:<5}");
    let styled_level = match level {
        0 | 1 => style.dim(&padded),
        3 => style.yellow(&padded),
        4 => style.red(&padded),
        _ => style.green(&padded),
    };
    let (message, category, exception) = structured.map_or((fields, None, None), |record| {
        (
            record.message.as_str(),
            Some(record.category.as_str()),
            record.exception.as_deref(),
        )
    });
    let continuation = "         ";
    let message = terminal_text(message).replace('\n', &format!("\n{continuation}"));
    let category = category.map_or_else(String::new, |category| {
        style.dim(&terminal_text(category).replace('\n', " "))
    });
    let exception = exception.map_or_else(String::new, |exception| {
        format!(
            "\n{continuation}{}",
            terminal_text(exception).replace('\n', &format!("\n{continuation}"))
        )
    });
    let category = if category.is_empty() {
        category
    } else {
        format!("  [{category}]")
    };
    format!("  {styled_level}  {message}{category}{exception}")
}

fn level_label(level: u8, declared: Option<&str>) -> &'static str {
    match (level, declared) {
        (4, Some("Critical")) => "crit",
        (0, _) => "trace",
        (1, _) => "debug",
        (2, _) => "info",
        (3, _) => "warn",
        (4, _) => "error",
        _ => "log",
    }
}

fn terminal_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\n' || character == '\t' || !character.is_control() {
            output.push(character);
        } else {
            output.extend(character.escape_default());
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::*;

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn normal_development_prints_only_readable_application_logs() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = {
            let captured = captured.clone();
            move || Buffer(captured.clone())
        };
        let subscriber =
            tracing_subscriber::registry().with(ApplicationLogLayer::new(writer, false));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "wisp_engine::artifact", composed = 3, "composed the adapter");
            tracing::info!(target: GUEST_LOG_TARGET, level = 1u64, fields = "wisp_handle enter: GET /", "guest log");
            let fields = serde_json::json!({
                "category": "CustomerApplication",
                "severity": "Information",
                "eventId": 7,
                "message": "Saved item 42",
                "properties": {},
                "scopes": []
            })
            .to_string();
            tracing::info!(target: GUEST_LOG_TARGET, level = 2u64, %fields, "guest log");
            tracing::info!(target: GUEST_LOG_TARGET, level = 4u64, fields = "console error", "guest log");
        });
        let output =
            String::from_utf8(captured.lock().clone()).expect("application log output is UTF-8");
        assert_eq!(
            output,
            "  info   Saved item 42  [CustomerApplication]\n  error  console error\n",
            "normal development must omit engine and SDK lifecycle details while retaining application output"
        );
    }

    #[test]
    fn framework_debug_logs_remain_visible_without_exposing_plain_sdk_debug_messages() {
        let fields = serde_json::json!({
            "category": "CustomerApplication",
            "severity": "Debug",
            "message": "cache miss",
            "exception": "first line\nsecond line"
        })
        .to_string();
        let line = GuestLogFields {
            level: Some(1),
            fields: Some(fields),
        }
        .render(false);
        assert_eq!(
            line.as_deref(),
            Some("  debug  cache miss  [CustomerApplication]\n         first line\n         second line"),
            "structured application debug records must remain readable"
        );
        assert!(
            GuestLogFields {
                level: Some(1),
                fields: Some("parking: pending futures=1".into()),
            }
            .render(false)
            .is_none(),
            "plain SDK lifecycle debug records must stay out of application output"
        );
    }

    #[test]
    fn application_text_cannot_write_terminal_control_sequences() {
        assert_eq!(
            terminal_text("before\u{1b}[2J\rafter"),
            "before\\u{1b}[2J\\rafter",
            "application text must escape terminal controls while retaining printable content"
        );
    }
}
