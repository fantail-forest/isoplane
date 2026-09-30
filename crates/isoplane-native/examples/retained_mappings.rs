//! Internal Linux mapping microbenchmark: unique overlays over one shared baseline.
#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
#[path = "retained_mappings/linux.rs"]
mod linux;

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::measure()
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("retained_mappings requires Linux mapping support and /proc memory metrics")
    }
}
