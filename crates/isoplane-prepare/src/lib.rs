// On Linux, preparation loads a published runtime baseline's native code.
#![cfg_attr(not(target_os = "linux"), forbid(unsafe_code))]

mod initialize;
mod instrument;
mod network;
mod ownership;
pub mod package;
pub use network::{InitializationDestinations, InitializationNetwork};
pub use ownership::PreparationOwner;

pub use initialize::{Initialization, Initialized};

/// Preparation failures classified independently of guest error text.
#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    /// Preparation was explicitly cancelled.
    #[cfg(target_os = "linux")]
    #[error("preparation cancelled")]
    Cancelled,
    /// A configured initialization backstop was exceeded.
    #[error("preparation limit: {0}")]
    Limit(&'static str),
    /// The module or the request was refused before anything ran.
    // The cause is part of each of these messages and not also a source: a
    // report that prints an error with its sources would otherwise repeat it.
    #[error("reading inputs failed: {0:#}")]
    Input(anyhow::Error),
    /// The engine the request describes could not be constructed.
    #[error("constructing the engine failed: {0:#}")]
    Engine(anyhow::Error),
    /// The prepared artifact could not be encoded.
    #[error("encoding the prepared artifact failed: {0:#}")]
    Artifact(anyhow::Error),
    /// Local preparation files could not be accessed.
    #[error("preparation I/O: {0}")]
    Io(#[source] std::io::Error),
    /// Request metadata or a state-equivalence invariant was invalid.
    #[error("invalid preparation request: {0}")]
    InvalidRequest(&'static str),
    /// The input or captured state could not be represented safely.
    #[error("preparation state: {0:#}")]
    State(anyhow::Error),
    /// Initialization did not return a successful empty-stack boundary.
    #[error("initializer ended with {completion:?}: {detail}")]
    Initialization {
        completion: isoplane_engine::Completion,
        detail: String,
    },
    /// Initialization called an import it is not permitted.
    #[error(
        "initialization called {import}, which an application may not call while it \
         initializes; move that operation out of initialization: {detail}"
    )]
    ForbiddenCapability {
        import: String,
        detail: String,
        completion: isoplane_engine::Completion,
    },
    /// A runtime's own initialization did what a baseline shared by every
    /// application on that runtime must not.
    #[error("runtime baseline {0}")]
    Baseline(&'static str),
    /// Prepared state could not be encoded.
    #[error("encoding preparation state: {0}")]
    Encoding(#[source] postcard::Error),
}
mod profile;
pub use profile::RuntimeProfile;
mod request;
pub use request::{BuildRequest, Module};

mod product;
pub use product::BuildProduct;

#[cfg(target_os = "linux")]
mod provider;
#[cfg(target_os = "linux")]
pub use provider::{LocalPreparation, Preparation};

/// The module preparation compiles for `wasm`: `wasm` itself, or, when it has an initializer,
/// `wasm` rewritten so that what initialization leaves can be captured. A compile of this module
/// in a node's compilation cache is what that node's preparations reuse.
#[cfg(any(test, feature = "test-util"))]
pub fn compiled_module(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    instrument::instrument(wasm)
}

mod baseline;
#[cfg(target_os = "linux")]
pub use baseline::PublishedBaseline;
pub use baseline::{BaselineRequest, PreparedApplication, PreparedBaseline, RuntimeBaseline};

pub mod preinit;
