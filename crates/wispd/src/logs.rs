//! Structured diagnostic output.

use tracing_subscriber::fmt::MakeWriter;

/// Installs diagnostic output through `writer`.
pub fn init<W>(json: bool, writer: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,wispd=debug,wisp_proxy=debug")
    });
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer);
    if json {
        let _ = builder.json().with_current_span(true).try_init();
    } else {
        let _ = builder.try_init();
    }
}
