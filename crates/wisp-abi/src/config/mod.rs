//! `wisp.toml`: the app manifest, its resource limits, and
//! the node-level facts the daemon and the command share.

mod lifecycle;
mod limits;
mod manifest;
mod memory;
mod node;

pub use lifecycle::LifecycleTier;
pub use limits::{Limits, ResourceLimitOverrides};
pub use manifest::{
    AppManifest, Bindings, HandlerConfig, HandlerLifecycle, InitSandbox,
};
pub use memory::{GuestMemoryBytes, InvalidGuestMemory, WASM32_MEMORY_MAX_BYTES};
pub use node::{DEFAULT_ADMIN_PORT, DEFAULT_PROXY_PORT};

mod validation;
pub use validation::{
    ValidationLimits, DEFAULT_COMPILER_STACK_BYTES, DEFAULT_CORE_INSTANCE_SIZE_BYTES,
    DEFAULT_TABLES_PER_MODULE, DEFAULT_TABLE_ELEMENTS, DEFAULT_WASM_STACK_BYTES,
};
