#![forbid(unsafe_code)]
#[cfg(target_os = "linux")]
#[path = "concurrent_pairs/phase_profile.rs"]
mod phase_profile;

use tracing_subscriber::prelude::*;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("supply the experiment specification"))?;
    #[cfg(target_os = "linux")]
    let profile = std::env::var_os("ISOPLANE_PHASE_PROFILE")
        .map(phase_profile::Profile::open)
        .transpose()?;
    let output = tracing_subscriber::fmt::layer()
        .json()
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE);
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with(output);
    #[cfg(target_os = "linux")]
    let subscriber = subscriber.with(profile.clone());
    subscriber.init();
    let result = isoplane_native::run_concurrent_pairs(std::path::Path::new(&path));
    #[cfg(target_os = "linux")]
    if let Some(profile) = profile {
        profile.finish()?;
    }
    result
}
