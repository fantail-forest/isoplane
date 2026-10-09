//! Restricted application initialization, separate from execution and deployment coordination.

mod bake;
mod initialize;
mod network;
mod ownership;
pub mod package;
pub use network::{InitializationDestinations, InitializationNetwork};
pub use ownership::PreparationOwner;

pub use initialize::{initialize, Initialization, Initialized, InitializedModule};

/// Preparation failures classified independently of guest error text.
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    /// Preparation was explicitly cancelled.
    #[error("preparation cancelled")]
    Cancelled,
    /// A configured initialization backstop was exceeded.
    #[error("preparation limit: {0}")]
    Limit(&'static str),
    /// Preparation returned a structured refusal.
    #[error("preparation rejected: {0}")]
    Rejected(WorkerFailure),
    /// Local preparation files could not be accessed.
    #[error("preparation I/O: {0}")]
    Io(#[source] std::io::Error),
    /// Request metadata or a state-equivalence invariant was invalid.
    #[error("invalid preparation request: {0}")]
    InvalidRequest(&'static str),
    /// The input or captured state could not be represented safely.
    #[error("preparation state: {0}")]
    State(#[source] anyhow::Error),
    /// Initialization did not return a successful empty-stack boundary.
    #[error("initializer ended with {completion:?}: {detail}")]
    Initialization {
        completion: isoplane_engine::Completion,
        detail: String,
    },
    /// A forbidden capability was reached outside any deferrable initialization unit.
    #[error("initialization reached {import} outside any unit: {detail}")]
    Unattributable {
        import: String,
        detail: String,
        completion: isoplane_engine::Completion,
    },
    /// A guest reran a unit that the sandbox told it to defer.
    #[error("initialization unit {unit} ignored deferral and called {import}")]
    DeferralIgnored { unit: u32, import: String },
    /// A runtime's own initialization did what a baseline shared by every
    /// application on that runtime must not.
    #[error("runtime baseline {0}")]
    Baseline(&'static str),
    /// Replays exhausted their configured pass limit.
    #[error("initialization did not converge within {0} passes")]
    NoConvergence(u32),
    /// Prepared state could not be encoded.
    #[error("encoding preparation state: {0}")]
    Encoding(#[source] postcard::Error),
}
mod profile;
pub use profile::RuntimeProfile;
mod process;
pub use process::{BuildRequest, PortableTemplate};

mod failure;
pub use failure::{BuildStage, WorkerFailure};

mod product;
pub use product::{prepare_in_current_process, BuildProduct, LocalPreparation, Preparation};

mod baseline;
#[cfg(target_os = "linux")]
pub use baseline::PublishedBaseline;
pub use baseline::{BaselineRequest, PreparedApplication, PreparedBaseline, RuntimeBaseline};

pub mod preinit;
pub use preinit::{PreInitError, PreInitOutcome, PreInitRequest, PreInitializer};
