//! The Isoplane contract: the guest ABI ([`status`], [`dispatch`], [`result`],
//! [`calls`]), the capability table ([`imports`], [`pal`]), the app manifest
//! and resource limits ([`config`]), and HTTP dispatch interfaces ([`host`], behind the
//! `host` feature).
//!
//! Builds for `wasm32-unknown-unknown` so a guest SDK can share these types
//! with the host instead of re-implementing the encoding.

pub mod calls;
pub mod config;
pub mod dispatch;
pub mod exports;
pub mod filesystem;
pub mod imports;
pub mod native_io;
pub mod pal;
pub mod result;
pub mod status;

#[cfg(feature = "host")]
pub mod host;

pub use dispatch::{DispatchKind, DispatchRecord, HttpRequest, ResponseHead};
pub use result::{FutureError, ResultRecord};
pub use status::{PollStatus, TrapCode};

/// Custom section identifying artifacts restricted to a local debugger session.
pub const LOCAL_DEBUG_SECTION: &str = "isoplane.local-debug";

/// The version of the guest host-interface contract.
pub const ABI_REV: u32 = 6;

/// The unit of copy-on-write sharing and of the hibernation page diff.
pub const PAGE_SIZE: usize = 4096;

/// The WASM linear-memory page size.
pub const WASM_PAGE_SIZE: usize = include!(concat!(env!("OUT_DIR"), "/wasm_page_bytes.rs"));

pub fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(v)
}

pub fn decode<'a, T: serde::Deserialize<'a>>(b: &'a [u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(b)
}
