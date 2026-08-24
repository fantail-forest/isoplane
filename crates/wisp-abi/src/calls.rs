//! Postcard-encoded argument payloads for the buffer-passing imports.
//!
//! Guest -> host data crosses as a `(ptr, len)` pair that is valid only for
//! the duration of the import call (§5.5). No host pointer ever enters guest
//! memory, and the host copies out before returning.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

















