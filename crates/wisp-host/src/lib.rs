//! The cell: the runtime data plane that owns kernel objects, runs guest code
//! and holds the memory that is the tenant's state.
//!
//! Two thread classes. The cell's tokio reactor threads own all I/O: a
//! completion arrives, [`futures::FutureTable`] maps its id to the owning
//! instance, and a wake is enqueued. Execution-pool threads run Hot dispatches
//! as synchronous calls into the engine, never the engine's async mode, whose
//! epoch yielding runs on fibers, hidden native stacks that a parked instance
//! could not be captured out of. There is no scheduler here: a CPU-bound
//! tenant occupies its own pool thread and the kernel preempts it. What the
//! cell controls is admission, a bounded queue per worker and a shed when it
//! is full.
//!
//! Two guarantees are carried by the type system rather than by review. The
//! only `&mut dyn InstanceOps` in this crate comes from
//! [`slot::CallGuard`], obtained by moving the instance out of its slot,
//! so a reentrant call finds an empty slot. And a `fresh` instance is reset or
//! discarded after its dispatch because [`disposition::Disposition`] has one
//! producer, [`disposition::decide`], and no value of it means "keep a fresh
//! instance warm".
//!
//! A cell is a thread group within one process, not a seccomp'd process, and
//! fairness between tenants is whatever the kernel gives equal-weight threads.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod activator;
pub mod bindings;
pub mod breaker;
pub mod cell;
pub mod cpu_clock;
pub mod debug;
pub mod dispatcher;
pub mod disposition;
pub mod egress;
pub mod fakes;
pub mod fetcher;
pub mod futures;
pub mod meters;
pub mod metrics;
pub mod pool;
pub mod response;
mod response_transport;
mod sharded_map;
pub mod slot;
pub use activator::EngineActivator;
pub use bindings::{CellBindings, InstanceCtx};
pub use breaker::{BreakerState, HangBreaker};
pub use cell::{Cell, CellConfig, DispatchResult, Services, Target};
pub use disposition::Disposition;
pub use egress::{DenyReason, HostEgressPolicy, Resolver};
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

/// Cell-local identity of a slot. A migrated instance gets a new one, because
/// instances are portable memory, not portable identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
/// `None` is a failed read, counted in [`meters::clock_read_failures`]; every
/// consumer subtracts on unsigned times, so a read flattened to zero would
/// produce a duration near 2^64 rather than a missing one.
pub fn mono_ns() -> Option<u64> {
    meters::try_read_mono_ns()
}

/// Native CPU time for the calling thread: the sole compute meter.
pub use meters::read_thread_cpu_ns as thread_cpu_ns;
