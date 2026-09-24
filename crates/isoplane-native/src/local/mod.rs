//! Local execution using the same preparation, host and proxy as the daemon.

pub mod debug;
mod destinations;
mod runtime;

pub use runtime::{Baselines, LocalApp, Prepared};

/// Resolved application inputs; callers own project discovery and publication.
#[derive(Clone)]
pub struct Inputs {
    /// Application capabilities and resource limits.
    pub manifest: isoplane_abi::config::AppManifest,
    /// Private local persistent state and preparation cache.
    pub state_directory: std::path::PathBuf,
    /// The loaded native implementation, included in the preparation identity.
    pub runtime_library: std::path::PathBuf,
    /// Compiler-owned storage selected by the CLI outside application source control.
    pub compilation_directory: std::path::PathBuf,
    /// Initialization capability policy supplied by the SDK.
    pub init_sandbox: isoplane_abi::config::InitSandbox,
    /// Explicitly authorized local service destinations.
    pub allow_endpoints: Vec<std::net::SocketAddr>,
    /// Host-provided files visible to the sandbox.
    pub platform_files: isoplane_engine::PlatformFiles,
    /// The application package's files, for a module that runs a package
    /// rather than carrying its application; empty otherwise.
    pub application_files: isoplane_engine::PlatformFiles,
}

impl Inputs {
    /// Every file the sandbox sees while it has no template to bind
    /// application files to: during initialization and under a debugger.
    fn served_files(
        &self,
    ) -> Result<isoplane_engine::PlatformFiles, isoplane_engine::PlatformFilesError> {
        self.platform_files.merge(&self.application_files)
    }

    fn tenant(&self) -> String {
        let identity = blake3::hash(self.state_directory.as_os_str().as_encoded_bytes());
        format!("local:{identity}")
    }
}

/// Failure to prepare or start a local application.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native compilation, admission or initialization failed.
    #[error("local preparation failed: {0:#}")]
    Preparation(#[source] anyhow::Error),
    /// Local host services could not be started.
    #[error("local host startup failed: {0:#}")]
    Startup(#[source] anyhow::Error),
    /// A debugger peer did not identify a bound loopback endpoint.
    #[error("the debugger endpoint must be the session-owned 127.0.0.1 address with a bound port")]
    DebugEndpoint,
}
