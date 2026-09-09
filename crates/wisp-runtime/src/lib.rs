//! Guest admission, template preparation and host bindings shared by local development and the daemon.

#![forbid(unsafe_code)]

#[cfg(feature = "preinit")]
pub mod preinit;
pub mod validator;

#[cfg(feature = "preinit")]
pub use preinit::{PreInitError, PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};

pub mod network;
#[cfg(feature = "local")]
pub mod sockets;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
