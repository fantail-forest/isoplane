//! Node configuration and measurements.

pub mod admin;
pub mod cli;
pub mod config;
#[cfg(feature = "real-backends")]
pub mod deploy;
pub mod logs;
pub mod metrics;
pub mod node;
#[cfg(feature = "real-backends")]
pub mod preparation;
#[cfg(feature = "real-backends")]
pub mod real;
#[cfg(feature = "real-backends")]
mod reclamation;
pub use isoplane_trust as trust;

pub use config::{ConfigError, NodeConfig};
pub use node::DeployStage;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(feature = "real-backends")]
pub mod development;
