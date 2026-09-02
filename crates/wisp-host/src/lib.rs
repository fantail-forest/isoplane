//! # `wisp-host` — the cell
//!
//! A cell is the runtime data plane: the thing that owns kernel objects, runs
//! guest code, and holds the memory that *is* the tenant's state. Three
//! properties are what everything here exists to make operationally true.
//!
//! **Two thread classes per cell.** The cell's tokio reactor threads own all
//! I/O — a completion arrives, the [`futures::FutureTable`] maps future id →
//! owning instance, and the wake is enqueued. Execution-pool threads ([`cell::Cell`]) run
//! Hot dispatches as *synchronous* calls into the engine. Deliberately not the
//! engine's async mode: its epoch-yielding is implemented with fibers, i.e.
//! hidden native stacks, which invariant 3 forbids. A native stack exists only
//! while a dispatch is Hot.
//!
//! **Fairness is the kernel's job.** There is no scheduler in this
//! crate. A CPU-bound tenant occupies its own pool thread and the OS preempts
//! it; that is the design and it is the incumbent's real contract. What the
//! cell *does* control is admission: a cap on concurrently Hot dispatches, an
//! overflow queue, and shedding before saturation.
//!
//! **Eviction is legal from Parked at any time.** [`instances`] is a
//! real state machine over Hot / Parked / Hibernated / Discarded, and
//! [`hibernate_policy`] is the idle trajectory that walks instances down it
//! with no tenant involvement.
//!
//! ## What the type system is made to carry
//!
//! Two guarantees are the kind that decay into code-review folklore unless the
//! compiler holds them, so they are encoded rather than documented:
//!
//! * **Reentrancy** — the host must not call into an instance while a previous
//!   call into it has not returned. The only `&mut dyn
//!   InstanceOps` in this crate comes from [`instances::CallGuard`], which is
//!   obtained by *moving* the instance out of its slot. A second entry finds
//!   an empty slot. Violating the rule is not discouraged, it is
//!   unrepresentable.
//! * **The `fresh` guarantee** — a `fresh` instance is reset or discarded after
//!   the dispatch and any `WaitUntil` work, with no exceptions. The caller of a
//!   dispatch never chooses the disposition:
//!   [`instances::DispatchLease::finish`] consumes the lease and derives it.
//!   There is no API that returns a `fresh` instance to the warm pool, which is
//!   what makes the guarantee structural rather than a hope.
//!
//! ## Deviations
//!
//! * **tokio, not io_uring.** The reactor role is a tokio runtime. The
//!   architecture — reactors own I/O and route wakes, exec-pool threads run
//!   dispatches — is the part that matters.
//! * **Cells are thread groups, not processes.** The seccomp'd process
//!   boundary is simulated; the trust-tier partitioning logic is real.
//! * **Per-tenant CPU weighting is absent.** cgroup v2 threaded controllers
//!   would supply it. Fairness here is whatever the OS scheduler gives a set of
//!   equal-weight threads — statistical, kernel-grade fairness, but with no
//!   per-tenant weighting. See [`cell::CellConfig::exec_threads`].

// Two places in this crate are `unsafe`, and both are excused by name rather
// than by relaxing the rule: `meters::read_clock_ns`, for `clock_gettime`, and
// `activator::EngineActivator::load_mapped`, which maps a compiled artifact
// from a file and therefore carries wasmtime's own `deserialize_file`
// contract. Everything else is denied.
#![deny(unsafe_code)]

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

pub mod activator;
pub mod bindings;
pub mod breaker;
pub mod cell;
pub mod dispatcher;
pub mod egress;
pub mod fakes;
pub mod futures;
pub mod instances;
pub mod meters;
pub mod metrics;
pub mod net;
pub mod response;
mod sharded_map;
pub use activator::EngineActivator;
pub use bindings::{CellBindings, InstanceCtx};
pub use breaker::{BreakerState, HangBreaker};
pub use cell::{Cell, CellConfig, DispatchResult, Services, Target};
pub use egress::{DenyReason, HostEgressPolicy, Resolver};
pub use futures::{FutureTable, PendingCapError};
pub use instances::{Activator, CallGuard, Disposition, LifecycleTier};
pub use meters::Meters;
pub use metrics::MetricsSnapshot;
pub use net::PinnedHttpFetcher;

pub type Result<T> = anyhow::Result<T>;

/// A deployed module version. A deploy invalidates memory: templates are
/// per-version, so this is also the identity a hibernation delta is bound to
/// and the key the hang breaker cools per.
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

/// Cell-local identity of a slot. Not stable across cells — a migrated
/// instance gets a new one, which is fine because instances are portable
/// *memory*, not portable identities.
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

/// Coarse wall clock in milliseconds. Hosts may use real time; guests get the
/// frozen slice clock instead — the whole point being that the guest cannot
/// construct a timer.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Raw `CLOCK_MONOTONIC` nanoseconds, with no per-process origin subtracted,
/// so a reading here is on the same scale as a bare `clock_gettime` anywhere
/// else in the process.
///
/// Aborting on an unreadable clock is deliberate: every consumer subtracts on
/// **unsigned** times, so a failed read flattened to `0` does not produce a
/// missing duration, it produces one near 2^64 in whichever direction hurts
/// most. `try_mono_ns` is the form to use where "could not read" is a fact the
/// caller wants to hold.
pub fn mono_ns() -> u64 {
    match try_mono_ns() {
        Some(t) => t,
        None => mono_clock_failed(),
    }
}

/// The fallible read. `None` is a failed `clock_gettime`; `Some(0)` is a clock
/// that genuinely reads zero, and those are different facts.
pub fn try_mono_ns() -> Option<u64> {
    meters::try_read_mono_ns()
}

/// Out of line and `#[cold]`, so the hot path carries a never-taken branch.
#[cold]
#[inline(never)]
fn mono_clock_failed() -> ! {
    let errno = std::io::Error::last_os_error();
    eprintln!(
        "wisp-host: clock_gettime(CLOCK_MONOTONIC) did not return a usable time (errno: \
         {errno}). Every interval this cell reports is measured against that clock and every \
         consumer subtracts on unsigned times, so continuing would produce elapsed times near \
         2^64 rather than no times at all. Aborting instead."
    );
    std::process::abort();
}

/// `CLOCK_THREAD_CPUTIME_ID` for the calling thread. This is **the sole
/// compute meter**: exact under kernel preemption and immune to wall-clock
/// noise, which is exactly why park time need never be metered.
pub use meters::read_thread_cpu_ns as thread_cpu_ns;
