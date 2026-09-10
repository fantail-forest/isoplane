//! Shared guest ABI, capabilities, configuration and host dispatch contracts.
//! Resumable state comprises sandbox state and explicit host-owned resources.
//! Guests return on an empty stack, host-owned wake sources resume them, and
//! every external capability is declared in `imports::IMPORTS`.

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

pub use dispatch::{DispatchKind, DispatchRecord, HttpRequest, ResponseHead, };
pub use result::{FutureError, ResultRecord};
pub use status::{PollStatus, TrapCode};

/// Custom section identifying artifacts restricted to a local debugger session.
pub const LOCAL_DEBUG_SECTION: &str = "wisp.local-debug";

/// The version of the guest host-interface contract.
pub const ABI_REV: u32 = 1;

/// The unit of copy-on-write sharing and of the hibernation page diff.
pub const PAGE_SIZE: usize = 4096;

/// The WASM linear-memory page size.
pub const WASM_PAGE_SIZE: usize = 65_536;

pub fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(v)
}

pub fn decode<'a, T: serde::Deserialize<'a>>(b: &'a [u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(b)
}
