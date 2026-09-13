//! Structured diagnostic output and optional OTLP export.

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

mod diagnostics;
mod export;
pub use export::{EndpointError, OtlpEndpoint};

/// Logging setup or exporter shutdown failure.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("could not construct OTLP exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("could not install tracing subscriber: {0}")]
    Subscriber(#[from] tracing::subscriber::SetGlobalDefaultError),
    #[error("could not flush or stop OTLP export: {0:?}")]
    Shutdown(Vec<opentelemetry_sdk::error::OTelSdkError>),
}

/// Owns shutdown of the optional batch exporters.
pub struct Logging {
    provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl Logging {
    /// Flushes queued events and stops the exporter. Call outside an async reactor.
    pub fn shutdown(self) -> Result<(), LogError> {
        if let Some(provider) = self.provider {
            // Force-flush preserves export errors; batch shutdown only acknowledges worker exit.
            let errors: Vec<_> = [provider.force_flush(), provider.shutdown()]
                .into_iter()
                .filter_map(Result::err)
                .collect();
            if !errors.is_empty() {
                return Err(LogError::Shutdown(errors));
            }
        }
        Ok(())
    }
}

/// Installs diagnostic output through `writer` and optional OTLP export.
/// Construction uses a blocking HTTP client; call outside an async reactor.
pub fn init<W>(
    json: bool,
    writer: W,
    node: &str,
    endpoint: Option<&OtlpEndpoint>,
) -> Result<Logging, LogError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,wispd=debug,wisp_proxy=debug")
    });
    let (logging, subscriber) = build(json, writer, node, endpoint, filter)?;
    if let Err(error) = tracing::dispatcher::set_global_default(subscriber) {
        logging.shutdown()?;
        return Err(error.into());
    }
    Ok(logging)
}

fn build<W>(
    json: bool,
    writer: W,
    node: &str,
    endpoint: Option<&OtlpEndpoint>,
    filter: tracing_subscriber::EnvFilter,
) -> Result<(Logging, tracing::Dispatch), LogError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let safe = tracing_subscriber::filter::filter_fn(diagnostics::contains_no_collector_payload);
    let text = if json {
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_writer(writer)
            .with_filter(safe)
            .with_filter(filter)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_filter(safe)
            .with_filter(filter)
            .boxed()
    };
    let provider = endpoint
        .map(|endpoint| export::provider(node, endpoint))
        .transpose()?;
    let events = provider.as_ref().map(|provider| {
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(provider)
            .with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("wisp_host::cell::completion", tracing::Level::INFO),
            )
    });
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(text)
            .with(events)
            .with(diagnostics::CollectorDiagnostics.with_filter(
                tracing_subscriber::filter::filter_fn(|metadata| {
                    !diagnostics::contains_no_collector_payload(metadata)
                }),
            )),
    );
    Ok((Logging { provider }, subscriber))
}

#[cfg(test)]
#[path = "logs/export_tests.rs"]
mod export_tests;
