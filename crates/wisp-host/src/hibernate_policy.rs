//! The idle trajectory: a durable instance parked beyond its class's
//! threshold, or a socket-holding Handler parked beyond its own, is captured
//! with no tenant involvement. There is no import to request or refuse
//! hibernation, because the platform promises capture at no particular
//! moment; a durable instance that needs a durability point persists. A wake
//! rebuilds the instance from the template and applies the delta, which is
//! also how ratcheted RSS goes back to the kernel.

pub use crate::hibernate_capture::{CaptureEnv, CaptureObserver, CaptureRefused, GC_HINT_SNAPSHOT};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HibernateConfig {
    /// Captured delta size above which the host emits an operator warning.
    pub delta_advisory_bytes: u64,
}

impl Default for HibernateConfig {
    fn default() -> Self {
        Self {
            delta_advisory_bytes: 8 << 20,
        }
    }
}
