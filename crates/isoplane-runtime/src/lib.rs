//! Guest admission and host bindings shared by local development and the daemon.

#![forbid(unsafe_code)]

pub mod inspection;
pub mod validator;

pub use validator::{
    validate, validate_local_debug, validate_local_debug_with_limits, validate_with_limits,
    validate_with_memory_limit, ValidationReport, Violation, ViolationCode,
};

pub mod network;
