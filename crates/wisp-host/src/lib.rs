//! # `wisp-host` — the cell (spec.md §6)
//!
//! A cell is the runtime data plane: the thing that owns kernel objects, runs
//! guest code, and holds the memory that *is* the tenant's state. Everything
//! here exists to make three sentences from the spec operationally true.
//!
//! **"Two thread classes per cell"** (§6.1). The cell's tokio reactor threads own all
//! I/O — a completion arrives, the [`futures::FutureTable`] maps future id →
//! owning instance, and the wake is enqueued. Execution-pool threads ([`cell::Cell`]) run
//! Hot dispatches as *synchronous* calls into the engine. Deliberately not the
//! engine's async mode: its epoch-yielding is implemented with fibers, i.e.
//! hidden native stacks, which invariant 3 forbids. A native stack exists only
//! while a dispatch is Hot.
//!
//! **"Fairness is the kernel's job"** (§6.4). There is no scheduler in this
//! crate. A CPU-bound tenant occupies its own pool thread and the OS preempts
//! it; that is the design and it is the incumbent's real contract. What the
//! cell *does* control is admission: a cap on concurrently Hot dispatches, an
//! overflow queue, and shedding before saturation.
//!
//! **"Eviction is legal from Parked at any time"** (§6.5). [`instances`] is a
//! real state machine over Hot / Parked / Hibernated / Discarded, and
//! [`hibernate_policy`] is the idle trajectory that walks instances down it
//! with no tenant involvement.
//!
//! ## What the type system is made to carry
//!
//! Two of the spec's guarantees are the kind that decay into code-review
//! folklore unless the compiler holds them, so they are encoded rather than
//! documented:
//!
//! * **§5.4's reentrancy rule** — "the host MUST NOT call into an instance
//!   while a previous call into it has not returned". The only `&mut dyn
//!   InstanceOps` in this crate comes from [`instances::CallGuard`], which is
//!   obtained by *moving* the instance out of its slot. A second entry finds
//!   an empty slot. Violating the rule is not discouraged, it is
//!   unrepresentable.
//! * **§2.6's `fresh` guarantee** — "reset or discarded after the dispatch and
//!   any `WaitUntil` work, **guaranteed**". The caller of a dispatch never
//!   chooses the disposition: [`instances::DispatchLease::finish`] consumes
//!   the lease and derives it. There is no API that returns a `fresh` instance
//!   to the warm pool, which is what makes §2.6's selling point structural
//!   rather than a hope.
//!
//! ## Deviations (docs/plan.md)
//!
//! * **tokio, not io_uring.** The reactor role is a tokio runtime. The
//!   architecture — reactors own I/O and route wakes, exec-pool threads run
//!   dispatches — is what §6.1 is actually about.
//! * **Cells are thread groups, not processes.** §9.1's seccomp'd process
//!   boundary is simulated; the trust-tier partitioning logic (§8.2.4) is
//!   real.
//! * **Per-tenant CPU weighting via cgroup v2 threaded controllers (§6.4) is
//!   out of PoC scope.** Fairness here is whatever the OS scheduler gives a
//!   set of equal-weight threads, which is the statistical, kernel-grade
//!   fairness §6.4 promises — but the *weighting* half is absent. See
//!   [`cell::CellConfig::exec_threads`].

// One `unsafe` block exists in this crate, in `meters::read_thread_cpu_ns`,
// for `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`. Everything else is denied.
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
pub mod lockstats;
pub mod meters;
pub mod metrics;
pub mod net;
pub mod response;

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

/// A deployed module version. Templates are per-version (§9.4: "deploys
/// invalidate memory"), so this is also the identity a hibernation delta is
/// bound to and the key the hang breaker cools per.
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
/// *memory*, not portable identities (§8.2.4).
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
/// frozen slice clock instead (§8.5) — the whole point being that the guest
/// cannot construct a timer.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Monotonic nanoseconds since the first call, for stamping an instant that
/// another thread in the same process will difference against.
///
/// **Not a wall clock and not comparable across processes.** It exists because
/// [`cell::StageTimings::replied_mono_ns`] has to name an instant that the
/// receiving thread can subtract from its own reading, and `Instant` is
/// opaque: two of them subtract, but one cannot be put in a struct that
/// crosses a channel and be turned back into a duration by the reader without
/// an agreed origin. This is that origin.
///
/// `CLOCK_MONOTONIC` underneath, the same clock `Instant` reads, so a caller
/// timing the other end with either one is measuring against the same source.
pub fn mono_ns() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as u64
}

/// `CLOCK_THREAD_CPUTIME_ID` for the calling thread (§6.4, §11.2). This is
/// **the sole compute meter**: exact under kernel preemption and immune to
/// wall-clock noise, which is exactly why park time need never be metered.
pub use meters::read_thread_cpu_ns as thread_cpu_ns;
