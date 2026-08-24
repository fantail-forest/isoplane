//! Snapshot identity and page-size primitives.

use wisp_engine::ModuleHash;

/// Page size shared with the guest contract.
pub const PAGE: usize = wisp_abi::PAGE_SIZE;

/// Module and host-interface identity required by a saved snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Original module identity.
    pub module_hash: ModuleHash,
    /// Original host-interface revision.
    pub abi_rev: u32,
}

impl Binding {
    /// Names the module and host interface needed for restoration.
    pub fn new(module_hash: ModuleHash, abi_rev: u32) -> Self {
        Binding {
            module_hash,
            abi_rev,
        }
    }
}
