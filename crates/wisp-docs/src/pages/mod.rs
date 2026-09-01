//! One module per reference page this crate produces.
//!
//! [`cli`] emits its three pages **whole**; the rest replace marked regions and
//! leave the surrounding prose alone.
//!
//! What is deliberately absent is listed, with reasons, in
//! `tests/generated_reference_docs_do_not_drift.rs` — a page that is neither
//! generated nor on that list fails the build.

pub mod capabilities;
pub mod cli;
pub mod guest_abi;
pub mod metrics;
pub mod sdk_csharp;
pub mod sdk_rust;
pub mod validator;
pub mod wisp_toml;
