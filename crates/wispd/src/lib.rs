//! Node configuration and measurements.

pub mod admin;
pub mod config;
pub mod logs;
pub mod metrics;
pub mod node;
#[cfg(feature = "real-backends")]
pub mod real;

pub use config::{ConfigError, NodeConfig};
pub use node::DeployStage;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
