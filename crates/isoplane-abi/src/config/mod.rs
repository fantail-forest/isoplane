//! The app manifest, its resource budgets, and
//! the node-level facts the daemon and the command share.

mod lifecycle;
mod limits;
mod manifest;
mod memory;
mod node;

pub use lifecycle::LifecycleTier;
pub use limits::{Limits, ResourceLimitOverrides, DEFAULT_INSTANCE_OPEN_CONNECTIONS};
pub use manifest::{AppManifest, Bindings, InitSandbox};
pub use memory::{
    memory_ceiling_bytes, GuestMemoryBytes, InvalidGuestMemory, WASM32_MEMORY_MAX_BYTES,
};
pub use node::{DEFAULT_ADMIN_PORT, DEFAULT_PROXY_PORT};

mod validation;
pub use validation::{
    ValidationLimits, DEFAULT_COMPILER_STACK_BYTES, DEFAULT_CORE_INSTANCE_SIZE_BYTES,
    DEFAULT_MAX_INSTANCES, DEFAULT_MEMORY_GUARD_BYTES, DEFAULT_MEMORY_LIMIT_BYTES,
    DEFAULT_TABLES_PER_MODULE, DEFAULT_TABLE_ELEMENTS, DEFAULT_WASM_STACK_BYTES,
};
