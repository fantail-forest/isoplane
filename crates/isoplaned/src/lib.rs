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

pub mod settings;

// The test suites' entry points must never reach a shipped build.
#[cfg(all(feature = "test-util", not(debug_assertions)))]
compile_error!("the `test-util` feature is for tests and must not be enabled in a release build");
