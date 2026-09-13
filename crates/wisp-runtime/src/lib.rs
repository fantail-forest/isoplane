//! Guest admission and host bindings shared by local development and the daemon.

#![forbid(unsafe_code)]

pub mod validator;

pub use validator::{
    validate, validate_local_debug, validate_local_debug_with_limits, validate_with_limits,
    validate_with_memory_limit, ValidationReport, Violation, ViolationCode,
};

pub mod network;
#[cfg(feature = "local")]
pub mod sockets;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
