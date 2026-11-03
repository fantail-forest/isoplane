//! Local multi-application host and deployment pipeline.

pub mod admin;
pub mod cli;
pub mod config;
pub mod deploy;
pub mod http;
pub mod logs;
pub mod metrics;
pub mod node;
pub mod preparation;
pub mod real;
mod reclamation;
pub mod runtimes;
pub use isoplane_trust as trust;

pub use config::{ConfigError, NodeConfig};
pub use node::{DeployStage, Node};
pub mod installed;

/// Unix wall-clock milliseconds for operator timestamps and lifecycle sweeps.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub mod settings;
