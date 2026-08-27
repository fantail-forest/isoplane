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

// Two places in this crate are `unsafe`, and both are excused by name rather
// than by relaxing the rule: `meters::read_clock_ns`, for `clock_gettime`, and
// the whole of `tsc_marks`, which is supplied code reading the CPU's timestamp
// counter through the `RDTSCP` intrinsic. Everything else is denied.
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
pub mod marks;
pub mod meters;
pub mod metrics;
pub mod net;
pub mod response;
pub mod trace;
/// Timestamp samples on x86-64, with an empty interface on other architectures.
#[cfg(target_arch = "x86_64")]
pub mod tsc_marks;
#[cfg(not(target_arch = "x86_64"))]
#[path = "tsc_marks_stub.rs"]
pub mod tsc_marks;

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

/// Raw `CLOCK_MONOTONIC` nanoseconds.
///
/// **One epoch, shared with every other crate in this process.** This used to
/// subtract a per-process origin captured on its own first call, which made a
/// reading here and a reading from `wisp_bench::schedule::mono_ns` — a bare
/// `clock_gettime(CLOCK_MONOTONIC)` — two numbers on two scales that look
/// exactly alike. Subtracting one from the other yields the machine's uptime
/// or, through a `saturating_sub`, a clean zero; neither reads as wrong.
///
/// The origin bought nothing. The load generator and the cell run in **one
/// process**, calling each other directly, so there was never a second process
/// for which an agreed origin would have been necessary — and there is no
/// second process now, so a raw reading is exactly as comparable and one
/// subtraction cheaper.
///
/// The per-request stage timings are now taken with the CPU's timestamp
/// counter rather than with this clock (see [`marks`]), so this is no longer
/// on the measurement path. It is still what the cell stamps request
/// scheduling and its own coarse intervals with, and having those on the same
/// scale as everyone else's readings costs nothing and removes a whole class
/// of silent subtraction error.
///
/// Aborting on an unreadable clock is deliberate and is the same decision
/// `wisp_bench::schedule::mono_ns` takes for the same reason: every consumer
/// subtracts on **unsigned** times, so a failed read flattened to `0` does not
/// produce a missing duration, it produces one near 2^64 in whichever
/// direction hurts most. `try_mono_ns` is the form to use where "could not
/// read" is a fact the caller wants to hold.
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

/// `CLOCK_THREAD_CPUTIME_ID` for the calling thread (§6.4, §11.2). This is
/// **the sole compute meter**: exact under kernel preemption and immune to
/// wall-clock noise, which is exactly why park time need never be metered.
pub use meters::read_thread_cpu_ns as thread_cpu_ns;
