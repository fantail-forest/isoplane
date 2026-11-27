//! Guest admission and host bindings shared by local development and the daemon.

#![forbid(unsafe_code)]

pub mod validator;

pub use validator::{
    validate_local_debug_with_limits, validate_with_limits, ValidationReport, Violation,
    ViolationCode,
};

pub mod network;
