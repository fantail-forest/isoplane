//! Defaults for local initialization, and initialization of a module that
//! carries its whole application.

/// Default local initialization occupancy budget.
pub const DEFAULT_INITIALIZATION_OCCUPANCY_MS: u32 = 30_000;
/// Default local initialization wall backstop, including waits.
pub const DEFAULT_INITIALIZATION_WALL_MS: u32 = 60_000;
/// Default retained initialization log bytes.
pub const DEFAULT_INITIALIZATION_LOG_BYTES: usize = 64 * 1024;

#[cfg(feature = "rust-guests")]
mod self_contained;
#[cfg(feature = "rust-guests")]
pub use self_contained::{PreInitError, PreInitializer};
