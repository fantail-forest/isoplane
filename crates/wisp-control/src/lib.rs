//! Local WebAssembly module validation.

pub mod validator;

pub use validator::{validate, Violation, ViolationCode};

pub type Result<T> = anyhow::Result<T>;
