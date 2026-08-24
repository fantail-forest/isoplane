//! Content addresses for guest modules.

use serde::{Deserialize, Serialize};

/// Content address of the tenant's uploaded module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModuleHash(pub [u8; 32]);

impl ModuleHash {
    pub fn of(bytes: &[u8]) -> Self {
        ModuleHash(*blake3::hash(bytes).as_bytes())
    }
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Display for ModuleHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.hex()[..16])
    }
}
