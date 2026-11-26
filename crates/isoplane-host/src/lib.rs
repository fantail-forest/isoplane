use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod activator;
mod admission;
mod background;
pub mod bindings;
pub mod breaker;
pub mod cell;
pub mod debug;
pub mod dispatcher;
pub mod disposition;
pub mod egress;
pub mod entropy;
// Unit-test doubles implement this compilation's host types, including its controlled clock.
#[cfg(test)]
extern crate self as isoplane_host;
#[cfg(test)]
#[path = "../../isoplane-harness/src/fakes.rs"]
pub mod fakes;
#[cfg(feature = "rust-guests")]
pub mod fetcher;
pub mod futures;
pub mod hibernate_capture;
pub mod hibernate_policy;
pub mod memory_pressure;
pub mod meters;
pub mod metrics;
mod occupancy_clock;
pub mod pool;
pub mod reactor_accounting;
pub mod response;
mod response_transport;
mod sharded_map;
pub mod slot;
#[cfg(test)]
mod test_events;
pub mod threads;
pub use activator::{EngineActivator, ReloadedTemplateDiffers, TemplateReload};
pub use bindings::{CellBindings, InstanceCtx};
pub use breaker::HangBreaker;
pub use cell::{Cell, CellConfig, DispatchResult, Services, Target};
pub use disposition::Disposition;
pub use egress::{DenyReason, HostEgressPolicy, Resolver};
#[cfg(feature = "rust-guests")]
pub use fetcher::PinnedHttpFetcher;
pub use futures::{FutureTable, PendingCapError};
pub use meters::Meters;
pub use metrics::MetricsSnapshot;
pub use pool::Activator;
pub use slot::{CallGuard, LifecycleTier};

pub type Result<T> = anyhow::Result<T>;

/// A deployed module version. Templates are per version, so this is also the
/// identity a hibernation delta is bound to and the key the hang breaker
/// cools per.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId {
    pub app: String,
    pub version: String,
}

impl ModuleId {
    pub fn new(app: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            version: version.into(),
        }
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.app, self.version)
    }
}

/// Process-local identity allocated for an application instance.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct InstanceId(pub u64);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i{}", self.0)
    }
}

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

impl InstanceId {
    pub fn alloc() -> Self {
        InstanceId(NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed))
    }
}

/// Coarse wall clock in milliseconds. Guests never see it; they get the frozen
/// slice clock, so no timer is constructible in-guest.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Native monotonic nanoseconds, with no per-process origin subtracted.
/// `None` is a failed read; every
/// consumer subtracts on unsigned times, so a read flattened to zero would
/// produce a duration near 2^64 rather than a missing one.
pub fn mono_ns() -> Option<u64> {
    meters::try_read_mono_ns()
}

/// Maximum simultaneous request dispatches sharing one instance.
pub(crate) const DISPATCHES_PER_INSTANCE: usize = 8;
