//! Labelled timestamp boundaries for dispatch and queue measurements.
//! Labels identify stage and queue boundaries. Available labels depend on the
//! request path and queue implementation; absent labels are not zero intervals.
use crate::trace::{Spans, TracePool};
use crate::tsc_marks::Calibration;

/// The marks a request can carry, in the order a plain request meets them.
///
/// The name of a mark is the name of the **span that ends at it** — the
/// convention [`Marks::mark`] documents — so `LaneLockAcquired` names the wait
/// for the lane lock, not the instant of acquisition.
///
/// `Copy` and one byte. The whole enum exists so that a span can be named
/// without allocating on the dispatch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Mark {
    /// Origin timestamp when the caller releases its request.
    /// It is not an interval-ending mark.
    CallerReleased,
    /// The call arriving inside the cell.
    CellEntered,

    // --- admission ---------------------------------------------------------
    /// Building the `BreakerKey`: two `String` clones for the `ModuleId` plus
    /// the route.
    BreakerLockWanted,
    /// **Waiting for the hang breaker's `entries` lock.**
    BreakerLockAcquired,
    /// End of the hang-breaker admission check.
    BreakerChecked,
    /// Allocating the `DispatchJob`: six owned `String`/`Vec`s.
    JobBuilt,
    /// Choosing a lane. Costs differently per [`crate::cell::QueuePlacement`]
    /// — two shared-counter reads, one `fetch_add`, or one xorshift — and
    /// early-returns under a single-queue structure. Emitted under every
    /// configuration so the span before it is always the same span.
    PlacementDecided,

    // --- the enqueue, mutex structures -------------------------------------
    /// Nothing: adjacent to `PlacementDecided`. Present so the lane lock's
    /// wait has a left edge that is not also the placement decision.
    LaneLockWanted,
    /// **The lane lock's wait, isolated.** The number the whole exercise
    /// exists to separate from the queue wait.
    LaneLockAcquired,
    /// End of the producer queue critical section.
    LaneLockReleased,
    /// Waking a parked worker. Free when none is parked; a futex wake when one
    /// is, which is the case worth being able to see.
    WaiterNotified,

    // --- the enqueue, lock-free structure -----------------------------------
    /// One `SeqCst` read-modify-write on a cacheline every producer touches.
    PushRegistered,
    /// The `closed` check.
    ClosedChecked,
    /// The bounded ring's compare-and-swap, and the `SeqCst` decrement after
    /// it. A shed is decided inside this span — `push` hands the job straight
    /// back when the ring is full.
    RingPushed,

    // --- the queue ---------------------------------------------------------
    /// **The queue wait**, plus the producer's critical section and the
    /// consumer's own lock acquisition — see the module docs on handoffs.
    /// Workers being busy is the thing that stretches it.
    JobTaken,
    /// The rest of the pop, the previous job's `job_done`, and the
    /// `catch_unwind` and `Weak::upgrade` in `exec_loop`.
    DispatchStarted,

    // --- getting ready to run ----------------------------------------------
    /// Nothing; the left edge of the module table's read lock.
    ModulesLockWanted,
    /// **Waiting for the module table's read lock.**
    ModulesLockAcquired,
    /// End of accessing the module table.
    ModulesLockReleased,
    /// The 404 branch, where it is taken.
    InstanceAcquireBegan,
    /// Nothing; the left edge of the handler pool's idle lists.
    PoolLockWanted,
    /// End of consulting the handler's reusable-instance pools.
    PoolLockReleased,
    /// Nothing; the left edge of a hibernated instance's rehydration.
    HibernationWakeBegan,
    /// End of snapshot loading and instance reconstruction, including any nested activation.
    HibernationWakeEnded,
    /// Allocating an `InstanceId`, building the `InstanceCtx` and the bindings
    /// table — everything the cold path does *before* the engine is asked for
    /// an instance.
    ActivationBegan,
    /// End of a fresh sandbox activation from its template.
    ActivationEnded,
    /// Nothing; the left edge of the cell's instance index.
    IndexLockWanted,
    /// End of updating the resident-instance index during handler acquisition.
    IndexLockReleased,
    /// Nothing; the left edge of the live-sandbox table.
    ResidentsLockWanted,
    /// **Waiting for the live-sandbox table.**
    ResidentsLockAcquired,
    /// End of accessing the resident-instance table.
    ResidentsLockReleased,
    /// End of acquiring the request's instance.
    InstanceAcquireEnded,
    /// Nothing; the left edge of the `active` probe.
    ActiveProbeWanted,
    /// End of probing the active-instance index.
    ActiveProbeReleased,
    /// Nothing; the left edge of the future table.
    CancelAllocWanted,
    /// **Waiting for the global future table.**
    CancelAllocAcquired,
    /// End of allocating the cancellation future.
    CancelAllocReleased,
    /// Nothing; the left edge of the `active` insert.
    ActiveInsertWanted,
    /// End of inserting into the active-instance index.
    ActiveInsertReleased,

    // --- running the guest --------------------------------------------------
    /// Nothing; the left edge of the sandbox lock.
    SlotEnterWanted,
    /// **Waiting for the sandbox lock.** Nothing measured this lock before:
    /// it is a plain `parking_lot::Mutex`, not a `CountedMutex`, and it is in
    /// no lock table.
    SlotEnterAcquired,
    /// End of the slot entry transition.
    SlotEnterReleased,
    /// `hot.fetch_add`.
    GuestEntered,
    /// **The true cost of the guest call**, host seam included. Strictly
    /// larger than the engine's own `wall_ns`, which is measured inside the
    /// engine and misses `enter_hot`, `begin_slice`, the epoch deadline, trap
    /// classification and `exit_hot`. The engine's figure is kept beside this
    /// one; their difference is the host↔engine seam.
    GuestReturned,
    /// `hot.fetch_sub`; the left edge of the sandbox lock's second
    /// acquisition.
    SlotReleaseWanted,
    /// Releasing the sandbox: wait and hold together.
    SlotReleaseReleased,

    // --- settling -----------------------------------------------------------
    /// `entry.meters.record_cpu`, and the counter bumps at the top of `settle`.
    TenantRecordWanted,
    /// **Waiting for the per-tenant totals lock.** Not a `CountedMutex`, so
    /// nothing else in the cell measures it.
    TenantRecordAcquired,
    /// End of updating per-tenant totals.
    TenantRecordReleased,
    /// The trap latch.
    ActiveUpdateWanted,
    /// **Waiting for the in-flight record.**
    ActiveUpdateAcquired,
    /// End of updating request metadata in the active-instance index.
    ActiveUpdateReleased,
    /// Nothing; the left edge of the guest's response table.
    ResponseTableWanted,
    /// Waiting for the guest's response table. **Completed dispatches with a
    /// response handle only.**
    ResponseTableAcquired,
    /// Holding it — the whole response body may be cloned inside the lock, so
    /// this scales with the response size.
    ResponseTableReleased,
    /// Nothing; the left edge of the future table.
    WaitUntilCheckWanted,
    /// Waiting for the future table.
    WaitUntilCheckAcquired,
    /// Walking the `wait_until` set, one map lookup per element.
    WaitUntilCheckReleased,
    /// Nothing; the left edge of `build_result`'s read.
    ResultReadWanted,
    /// End of reading the active request result.
    ResultReadReleased,
    /// Nothing; the left edge of taking the responder.
    ResponderTakenWanted,
    /// End of taking the request responder.
    ResponderTakenReleased,
    /// The reply leaving the cell. Taken **after** the `active` shard is
    /// released and **before** the send, so the shard's own contention — which
    /// `lock_stats` reports separately — is not counted twice.
    ReplyHandedOff,
    /// **Time the cell was blocked because the reply channel was full.**
    ///
    /// `SyncSender::send` on a depth-one channel blocks, and the sender is an
    /// execution-pool thread — so a caller that stops draining stops the cell,
    /// and a caller that drains slowly holds a worker. This span was invisible
    /// to both ends until the trace stopped having a single owner: the buffer
    /// travels *inside* the result, so the thread doing the sending has
    /// nothing of its own left to mark with.
    SendReturned,

    // --- pausing and resuming ------------------------------------------------
    /// The guest returned `pending`: it is waiting for something and the
    /// worker has been handed back. Marks the park transition itself.
    Parked,
    /// Nothing; the left edge of the future table's readiness check.
    ReadyCheckWanted,
    /// The future table: wait and hold. A **pair** — one map lookup and an
    /// emptiness check.
    ReadyCheckReleased,
    /// **The request goes dormant.** The last mark of a slice that parked; the
    /// worker is free from here.
    SliceEnded,
    /// **Waiting for the outside world, plus the completion's own path back
    /// into the cell.**
    ///
    /// The single largest thing buried in the old instrument's residual, and
    /// the reason a parking guest reported as the fastest thing in the
    /// campaign: the old `guest_ns` timed the first slice only.
    ///
    /// The reading is taken where the wake is enqueued — on a reactor thread
    /// that holds the request's slot but not its trace — and filed by the
    /// worker that picks it up. Reaching the trace from the reactor would mean
    /// taking `Cell::active` on the wake path, and adding a lock to the wake
    /// path to measure the wake path is self-defeating.
    WakeEnqueueBegan,
    /// End of dequeueing a queued wake for execution.
    WakeStarted,
    /// Nothing; the left edge of the live-sandbox table on the wake path.
    WakeResidentsLockWanted,
    /// **Waiting for the live-sandbox table**, on the wake path.
    WakeResidentsLockAcquired,
    /// End of resident-instance access for a wake.
    WakeResidentsLockReleased,
    /// Nothing; the left edge of the future table.
    TakeReadyWanted,
    /// **Waiting for the future table** to hand over the completed result.
    TakeReadyAcquired,
    /// End of taking ready future completions.
    TakeReadyReleased,
    /// Nothing; the left edge of the budget read.
    ActiveBudgetWanted,
    /// End of reading the active request CPU budget.
    ActiveBudgetReleased,

    // --- refusals -------------------------------------------------------------
    /// The hang breaker said no. Refused without ever reaching a queue.
    BreakerRefused,
    /// The queue was full. **Under a mutex structure this happens while
    /// holding the lane lock, having already paid to acquire it** — a fact the
    /// old instrument threw away entirely; under the lock-free one the ring
    /// hands the job back and no lock is involved. Two different costs, and
    /// the span before this mark is what tells them apart.
    ShedDecided,
    /// No such module. Refused **after** queueing and being given a worker.
    ModuleMissing,
    /// Building or finding a sandbox failed. Refused after the most expensive
    /// part of setup was already paid for — possibly after a full activation.
    AcquireFailed,
    /// Label for a single-writer admission refusal.
    SingleWriterRefused,
    /// The sandbox refused entry: busy, or tombstoned and refusing everything.
    EnterRefused,

    // --- the caller's side again --------------------------------------------
    /// The reply crossing the channel and the collector being scheduled, plus
    /// any time the cell was blocked on a full channel. **The last mark**;
    /// `stop()` is taken here.
    CallerReceived,
}

/// Every mark, **in declaration order**, which is also discriminant order.
///
/// One list rather than several: the storage writes a label as a `u8` and the
/// reader turns it back, the report iterates the whole set to say which spans a
/// run did not observe, and the tests walk it to check the invariants. Three
/// separate lists would drift, and the way they would drift is a mark that
/// silently stops being reported.
pub const ALL: &[Mark] = &[
    Mark::CallerReleased,
    Mark::CellEntered,
    Mark::BreakerLockWanted,
    Mark::BreakerLockAcquired,
    Mark::BreakerChecked,
    Mark::JobBuilt,
    Mark::PlacementDecided,
    Mark::LaneLockWanted,
    Mark::LaneLockAcquired,
    Mark::LaneLockReleased,
    Mark::WaiterNotified,
    Mark::PushRegistered,
    Mark::ClosedChecked,
    Mark::RingPushed,
    Mark::JobTaken,
    Mark::DispatchStarted,
    Mark::ModulesLockWanted,
    Mark::ModulesLockAcquired,
    Mark::ModulesLockReleased,
    Mark::InstanceAcquireBegan,
    Mark::PoolLockWanted,
    Mark::PoolLockReleased,
    Mark::HibernationWakeBegan,
    Mark::HibernationWakeEnded,
    Mark::ActivationBegan,
    Mark::ActivationEnded,
    Mark::IndexLockWanted,
    Mark::IndexLockReleased,
    Mark::ResidentsLockWanted,
    Mark::ResidentsLockAcquired,
    Mark::ResidentsLockReleased,
    Mark::InstanceAcquireEnded,
    Mark::ActiveProbeWanted,
    Mark::ActiveProbeReleased,
    Mark::CancelAllocWanted,
    Mark::CancelAllocAcquired,
    Mark::CancelAllocReleased,
    Mark::ActiveInsertWanted,
    Mark::ActiveInsertReleased,
    Mark::SlotEnterWanted,
    Mark::SlotEnterAcquired,
    Mark::SlotEnterReleased,
    Mark::GuestEntered,
    Mark::GuestReturned,
    Mark::SlotReleaseWanted,
    Mark::SlotReleaseReleased,
    Mark::TenantRecordWanted,
    Mark::TenantRecordAcquired,
    Mark::TenantRecordReleased,
    Mark::ActiveUpdateWanted,
    Mark::ActiveUpdateAcquired,
    Mark::ActiveUpdateReleased,
    Mark::ResponseTableWanted,
    Mark::ResponseTableAcquired,
    Mark::ResponseTableReleased,
    Mark::WaitUntilCheckWanted,
    Mark::WaitUntilCheckAcquired,
    Mark::WaitUntilCheckReleased,
    Mark::ResultReadWanted,
    Mark::ResultReadReleased,
    Mark::ResponderTakenWanted,
    Mark::ResponderTakenReleased,
    Mark::ReplyHandedOff,
    Mark::SendReturned,
    Mark::Parked,
    Mark::ReadyCheckWanted,
    Mark::ReadyCheckReleased,
    Mark::SliceEnded,
    Mark::WakeEnqueueBegan,
    Mark::WakeStarted,
    Mark::WakeResidentsLockWanted,
    Mark::WakeResidentsLockAcquired,
    Mark::WakeResidentsLockReleased,
    Mark::TakeReadyWanted,
    Mark::TakeReadyAcquired,
    Mark::TakeReadyReleased,
    Mark::ActiveBudgetWanted,
    Mark::ActiveBudgetReleased,
    Mark::BreakerRefused,
    Mark::ShedDecided,
    Mark::ModuleMissing,
    Mark::AcquireFailed,
    Mark::SingleWriterRefused,
    Mark::EnterRefused,
    Mark::CallerReceived,
];

impl Mark {
    /// Recover a mark from the byte the trace stored.
    ///
    /// Indexing [`ALL`] rather than transmuting: `ALL` is checked against the
    /// discriminants by [`tests::the_label_table_matches_the_discriminants`],
    /// so a variant added without updating the list fails a test instead of
    /// silently renaming every span after it.
    #[inline]
    pub fn from_u8(v: u8) -> Option<Mark> {
        ALL.get(v as usize).copied()
    }
}

impl Mark {
    /// The label as it appears in a report row.
    ///
    /// Written out rather than derived from `Debug`, because these are read by
    /// a person looking for a lock by the name it has in the code, and because
    /// a rename of the variant must not silently rename a column in an
    /// archived record.
    pub fn name(self) -> &'static str {
        use Mark::*;
        match self {
            CallerReleased => "caller.released",
            CellEntered => "cell.entered",
            BreakerLockWanted => "breaker.check.wanted",
            BreakerLockAcquired => "breaker.check.wait",
            BreakerChecked => "breaker.check.hold",
            JobBuilt => "job.built",
            PlacementDecided => "queue.placement",
            LaneLockWanted => "lane.lock.wanted",
            LaneLockAcquired => "lane.lock.wait",
            LaneLockReleased => "lane.lock.hold",
            WaiterNotified => "lane.notify",
            PushRegistered => "ring.pushing.enter",
            ClosedChecked => "ring.closed.check",
            RingPushed => "ring.push",
            JobTaken => "queue.wait",
            DispatchStarted => "worker.start",
            ModulesLockWanted => "modules.read.wanted",
            ModulesLockAcquired => "modules.read.wait",
            ModulesLockReleased => "modules.read.hold",
            InstanceAcquireBegan => "acquire.begin",
            PoolLockWanted => "pool.idle.wanted",
            PoolLockReleased => "pool.idle",
            HibernationWakeBegan => "hibernation.wake.wanted",
            HibernationWakeEnded => "hibernation.wake",
            ActivationBegan => "activation.setup",
            ActivationEnded => "activation",
            IndexLockWanted => "index.lock.wanted",
            IndexLockReleased => "index.lock",
            ResidentsLockWanted => "residents.lock.wanted",
            ResidentsLockAcquired => "residents.lock.wait",
            ResidentsLockReleased => "residents.lock.hold",
            InstanceAcquireEnded => "acquire.rest",
            ActiveProbeWanted => "active.probe.wanted",
            ActiveProbeReleased => "active.probe",
            CancelAllocWanted => "futures.alloc.wanted",
            CancelAllocAcquired => "futures.alloc.wait",
            CancelAllocReleased => "futures.alloc.hold",
            ActiveInsertWanted => "active.insert.wanted",
            ActiveInsertReleased => "active.insert",
            SlotEnterWanted => "slot.enter.wanted",
            SlotEnterAcquired => "slot.enter.wait",
            SlotEnterReleased => "slot.enter.hold",
            GuestEntered => "guest.enter",
            GuestReturned => "guest",
            SlotReleaseWanted => "slot.release.wanted",
            SlotReleaseReleased => "slot.release",
            TenantRecordWanted => "tenant.record.wanted",
            TenantRecordAcquired => "tenant.record.wait",
            TenantRecordReleased => "tenant.record.hold",
            ActiveUpdateWanted => "active.update.wanted",
            ActiveUpdateAcquired => "active.update.wait",
            ActiveUpdateReleased => "active.update.hold",
            ResponseTableWanted => "responses.lock.wanted",
            ResponseTableAcquired => "responses.lock.wait",
            ResponseTableReleased => "responses.lock.hold",
            WaitUntilCheckWanted => "futures.waituntil.wanted",
            WaitUntilCheckAcquired => "futures.waituntil.wait",
            WaitUntilCheckReleased => "futures.waituntil.hold",
            ResultReadWanted => "active.read.wanted",
            ResultReadReleased => "active.read",
            ResponderTakenWanted => "active.responder.wanted",
            ResponderTakenReleased => "active.responder",
            Parked => "park.enter",
            ReadyCheckWanted => "futures.ready.wanted",
            ReadyCheckReleased => "futures.ready",
            SliceEnded => "slice.end",
            WakeEnqueueBegan => "park.wait",
            WakeStarted => "wake.queue.wait",
            WakeResidentsLockWanted => "wake.residents.wanted",
            WakeResidentsLockAcquired => "wake.residents.wait",
            WakeResidentsLockReleased => "wake.residents.hold",
            TakeReadyWanted => "futures.take.wanted",
            TakeReadyAcquired => "futures.take.wait",
            TakeReadyReleased => "futures.take.hold",
            ActiveBudgetWanted => "active.budget.wanted",
            ActiveBudgetReleased => "active.budget",
            BreakerRefused => "refused.breaker",
            ShedDecided => "refused.shed",
            ModuleMissing => "refused.no_module",
            AcquireFailed => "refused.acquire",
            SingleWriterRefused => "refused.single_writer",
            EnterRefused => "refused.enter",
            ReplyHandedOff => "reply.handoff",
            SendReturned => "reply.send.blocked",
            CallerReceived => "reply.delivery",
        }
    }

    /// Which queue structures this mark exists under.
    ///
    /// **Absent is not zero.** A `Shared` run has no `ring.push` observations
    /// and a `LockFree` run has no `lane.lock.wait` observations, and the
    /// report has to be able to say which of those it is looking at without
    /// inferring it from a missing row.
    pub fn structural(self) -> Structural {
        use Mark::*;
        match self {
            LaneLockWanted | LaneLockAcquired | LaneLockReleased | WaiterNotified => {
                Structural::MutexQueue
            }
            PushRegistered | ClosedChecked | RingPushed => Structural::LockFreeQueue,
            PoolLockWanted | PoolLockReleased | IndexLockWanted | IndexLockReleased => {
                Structural::HandlerTarget
            }
            HibernationWakeBegan | HibernationWakeEnded => Structural::HibernatedInstance,
            ActivationBegan | ActivationEnded => Structural::ColdAcquire,
            ResponseTableWanted | ResponseTableAcquired | ResponseTableReleased => {
                Structural::RespondingDispatch
            }
            Parked
            | ReadyCheckWanted
            | ReadyCheckReleased
            | SliceEnded
            | WakeEnqueueBegan
            | WakeStarted
            | WakeResidentsLockWanted
            | WakeResidentsLockAcquired
            | WakeResidentsLockReleased
            | TakeReadyWanted
            | TakeReadyAcquired
            | TakeReadyReleased
            | ActiveBudgetWanted
            | ActiveBudgetReleased => Structural::ParkingRequest,
            BreakerRefused | ShedDecided | ModuleMissing | AcquireFailed | SingleWriterRefused
            | EnterRefused => Structural::RefusedRequest,
            _ => Structural::Always,
        }
    }
}

/// What has to be true of a run for a mark to exist in it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Structural {
    /// Every request, every configuration.
    Always,
    /// [`crate::cell::QueueStructure::Shared`] and `PerWorker`.
    MutexQueue,
    /// [`crate::cell::QueueStructure::LockFree`].
    LockFreeQueue,
    /// The request targets an ordinary HTTP handler.
    HandlerTarget,
    /// A dispatch that had to build a new sandbox because there was none to
    /// reuse. On a steady-state run this is a handful of requests out of
    /// millions, and the row is still worth having: it is the expensive one.
    ColdAcquire,
    /// Label for an instance selected for snapshot restoration before entry.
    HibernatedInstance,
    /// A dispatch that completed with a response handle.
    RespondingDispatch,
    /// A request that paused and was resumed. Every span in this class repeats
    /// once per pause, and the ones inside `run_wake`'s own loop can repeat
    /// several times per pause.
    ParkingRequest,
    /// A request that was refused rather than served.
    RefusedRequest,
}

impl Structural {
    /// A sentence naming why a span is missing, for the report's prose.
    pub fn absence(self) -> &'static str {
        match self {
            Structural::Always => "should have been present under every configuration",
            Structural::MutexQueue => "does not exist under `lock-free`: there is no lane mutex",
            Structural::LockFreeQueue => {
                "does not exist under `shared` or `per-worker`: there is no ring"
            }
            Structural::HandlerTarget => {
                "only a handler dispatch reaches it: a durable instance is addressed by \
                 identity and has no pool"
            }
            Structural::ColdAcquire => {
                "only a dispatch that found nothing to reuse and had to build a sandbox \
                 reaches it"
            }
            Structural::HibernatedInstance => {
                "only a dispatch addressed to a hibernated instance reaches it"
            }
            Structural::RespondingDispatch => {
                "only a dispatch that completed with a response handle reaches it"
            }
            Structural::ParkingRequest => "only a request that paused and resumed reaches it",
            Structural::RefusedRequest => "only a refused request reaches it",
        }
    }
}

/// Timestamp-slot capacity for one sampled dispatch.
/// Exhaustion truncates the recorded prefix instead of extending the buffer during measurement.
pub const MARKS_CAPACITY: usize = 256;

/// A request's marks while it is in flight.
///
/// `None` on every request the sampler did not select and on every request
/// that arrived when the pool was empty, so the cost on the common path is one
/// `Option` discriminant.
pub type RequestMarks = crate::trace::Trace;

/// The pool a cell's requests check marks out of.
pub type RequestMarksPool = TracePool;

/// A handle for a thread that is about to give the request away and still has
/// timestamps to record. See [`crate::trace::Writer`].
pub type MarkWriter = crate::trace::Writer;

/// The handle for the enqueue's and the reply's tails, or `None` when the
/// request is untraced.
#[inline(always)]
pub fn writer(m: &Option<RequestMarks>) -> Option<MarkWriter> {
    m.as_ref().map(|t| t.writer())
}

/// Record a mark through a [`MarkWriter`].
#[inline(always)]
pub fn mark_after(w: &Option<MarkWriter>, label: Mark) {
    if let Some(w) = w {
        w.mark(label as u8);
    }
}

/// A counter reading taken where an event happened, to be filed into a trace
/// later by a thread that can reach it. See [`crate::trace::now`].
pub type Stamp = (u64, u32);

/// Read the counter now, for filing later.
#[inline(always)]
pub fn stamp() -> Stamp {
    crate::trace::now()
}

/// File a reading taken earlier.
#[inline(always)]
pub fn mark_at(m: &Option<RequestMarks>, label: Mark, at: Stamp) {
    if let Some(m) = m {
        m.mark_at(label as u8, at);
    }
}

/// Take a mark, if this request is carrying any.
///
/// A free function rather than a method so the call site reads as one line at
/// each of the fifty-odd places it appears, and `#[inline(always)]` so an
/// unsampled request pays a predictable branch and no call.
#[inline(always)]
pub fn mark(m: &Option<RequestMarks>, label: Mark) {
    if let Some(m) = m {
        m.mark(label as u8);
    }
}

/// Begin a request's trace at [`Mark::CallerReleased`].
///
/// Clears whatever the pooled buffer last held. Named `begin` rather than
/// `start` so that the call site says which instant it is taking: this is the
/// request's origin, and the first *span* is the one ending at
/// [`Mark::CellEntered`].
#[inline(always)]
pub fn begin(m: &Option<RequestMarks>) {
    if let Some(m) = m {
        m.begin();
    }
}

/// End a request's trace.
///
/// The same operation as [`mark`] — the shared storage has no distinguished
/// last slot, because it cannot know which thread will write it. Kept as its
/// own name so a call site says which of the two it is.
#[inline(always)]
pub fn stop(m: &Option<RequestMarks>, label: Mark) {
    mark(m, label)
}

/// Build a pool sized to a cell's admission bound.
///
/// `max_hot + admission_capacity` is the number of requests that can be inside
/// the cell at once — running, or queued — and the generator holds one more
/// per thread between checkout and submit. The slack is not generosity: a dry
/// pool is a request that goes untraced, counted in
/// [`MarksPool::exhausted_checkouts`], and a sample that thins out *under
/// load* biases every percentile in the flattering direction.
pub fn pool_for(
    cal: Calibration,
    max_hot: usize,
    admission: usize,
    slack: usize,
) -> RequestMarksPool {
    TracePool::new(cal, max_hot + admission + slack, MARKS_CAPACITY)
}

// ---------------------------------------------------------------------------
// Reading a finished trace
// ---------------------------------------------------------------------------

/// One finished request, reduced to spans.
///
/// Produced on the collector thread, off the dispatch path, once per traced
/// request.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    /// `(label, nanoseconds)` in the order the marks were taken.
    pub spans: Vec<(Mark, f64)>,
    /// **Last mark minus first, raw.** Not the sum of `spans`: each span has
    /// one mark's calibrated cost subtracted from it, so the sum is short by
    /// roughly `spans.len() * mark_overhead`. This is the number that must
    /// equal the caller's own send-to-receive measurement, and it is the one
    /// the coverage check uses.
    pub span_ns: f64,
    /// Whether any span has endpoints recorded with different processor tags.
    pub crossed_cores: bool,
    /// True if the buffer filled and the tail was dropped.
    pub truncated: bool,
    /// Slots claimed but not yet written when the trace was read — a writer
    /// from the far side of a handoff still in flight. Distinct from
    /// truncation, and counted separately because it has a different cause and
    /// a different fix.
    pub raced: usize,
}

/// Trace conditions counted independently so incomplete samples remain distinguishable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkFaults {
    /// The buffer filled and the tail was dropped. Raise [`MARKS_CAPACITY`],
    /// or accept that this workload parks more than the buffer allows.
    pub truncated: u64,
    /// The sequence of labels is not a legal path: a mark appeared out of the
    /// order the code can produce.
    pub malformed: u64,
    /// Intervals flagged as having out-of-order timestamps.
    pub out_of_order: u64,
    /// A trace that never reached its final mark. The wake path can drop a
    /// wake when a `Weak::upgrade` fails, which strands the request that was
    /// awaiting it; the platform now counts that as
    /// [`crate::metrics::Metrics::wakes_dropped`], and this is the same event
    /// seen from the instrument's side.
    pub abandoned: u64,
    /// Checkouts that found the pool empty, so the request went untraced.
    /// Non-zero means the pool is smaller than the real in-flight peak, and
    /// that the sample thinned out exactly when the cell was busiest.
    pub pool_exhausted: u64,
}

impl MarkFaults {
    pub fn any(&self) -> bool {
        self.truncated > 0
            || self.malformed > 0
            || self.out_of_order > 0
            || self.abandoned > 0
            || self.pool_exhausted > 0
    }
}

/// Reduces finished marks to stage intervals.
/// The uncorrected end-to-end interval is kept separately from overhead-corrected spans.
pub fn read(m: &RequestMarks) -> Trace {
    let Some(Spans {
        spans,
        span_ns,
        crossed_cores,
        truncated,
        raced,
    }) = crate::trace::read(m)
    else {
        return Trace::default();
    };
    Trace {
        spans: spans
            .into_iter()
            .filter_map(|(l, ns)| Mark::from_u8(l).map(|m| (m, ns)))
            .collect(),
        span_ns,
        crossed_cores,
        truncated,
        raced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every mark has a distinct name. Two marks sharing one would silently
    /// pool two different spans into one distribution, which is the same
    /// failure as summing a structural zero into a lock's average.
    #[test]
    fn no_two_marks_share_a_name() {
        let all = every_mark();
        let mut names: Vec<&str> = all.iter().map(|m| m.name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two marks share a name");
    }

    /// The two queue structures name **different** spans. If they ever came to
    /// share one, a report column comparing them would be comparing a lane
    /// mutex against a ring compare-and-swap.
    #[test]
    fn the_two_queue_structures_do_not_share_a_span() {
        let mutex: Vec<Mark> = every_mark()
            .into_iter()
            .filter(|m| m.structural() == Structural::MutexQueue)
            .collect();
        let free: Vec<Mark> = every_mark()
            .into_iter()
            .filter(|m| m.structural() == Structural::LockFreeQueue)
            .collect();
        assert!(!mutex.is_empty() && !free.is_empty());
        for a in &mutex {
            for b in &free {
                assert_ne!(a.name(), b.name());
            }
        }
    }

    /// Each structural class can say why its spans are missing. A row that
    /// vanishes without an explanation is exactly the shape the lock table
    /// already had to be fixed for.
    #[test]
    fn every_structural_class_explains_its_own_absence() {
        // Every class every mark actually claims, taken from the marks
        // themselves rather than from a list beside them — a class added to
        // the enum and forgotten here would otherwise go unexplained, which is
        // exactly the silence this test exists to prevent.
        let mut classes: Vec<Structural> = ALL.iter().map(|m| m.structural()).collect();
        classes.sort_unstable();
        classes.dedup();
        assert!(
            classes.len() >= 5,
            "only {} structural classes are reachable",
            classes.len()
        );
        for s in classes {
            assert!(
                !s.absence().is_empty(),
                "{s:?} cannot say why it is missing"
            );
        }
    }

    fn every_mark() -> Vec<Mark> {
        ALL.to_vec()
    }

    /// **The byte the trace stores is an index into `ALL`.**
    ///
    /// A variant inserted in the middle of the enum without being added to the
    /// list would shift every discriminant past it, and every span after that
    /// point would come back under its neighbour's name — a whole report,
    /// internally consistent, describing the wrong things.
    #[test]
    fn the_label_table_matches_the_discriminants() {
        for (i, m) in ALL.iter().enumerate() {
            assert_eq!(
                *m as u8 as usize, i,
                "`ALL[{i}]` is {m:?}, whose discriminant is not {i}"
            );
            assert_eq!(Mark::from_u8(i as u8), Some(*m));
        }
        assert_eq!(
            Mark::from_u8(ALL.len() as u8),
            None,
            "past the end is not a mark"
        );
    }

    /// A trace that fills the buffer stops marking rather than reallocating,
    /// and **says which** happened.
    ///
    /// The failure this prevents is invisible in the output: past its capacity
    /// a growable buffer reallocates inside the timed region, on the dispatch
    /// path, and the run that happens in reports a plausible number with a
    /// memcpy in the middle of it. Here the slots are fixed and the overflow is
    /// a counter, so a workload that pauses more than the buffer allows is
    /// reported as truncated rather than as fast.
    #[test]
    fn a_full_buffer_truncates_instead_of_growing() {
        let cal = Calibration::measure_for(std::time::Duration::from_millis(50));
        let pool = pool_for(cal, 1, 0, 0);
        let m = pool.checkout();
        assert!(m.is_some(), "a pool of one hands out one");
        begin(&m);
        for _ in 0..MARKS_CAPACITY * 2 {
            mark(&m, Mark::GuestReturned);
        }
        let t = read(m.as_ref().unwrap());
        assert!(
            t.truncated,
            "twice the capacity is a truncation and must say so"
        );
        assert_eq!(
            t.spans.len(),
            MARKS_CAPACITY,
            "it kept exactly the slots it had"
        );
        assert_eq!(t.raced, 0, "one thread wrote every slot it claimed");
    }

    /// The pool is sized to what can be in flight, and running it dry is
    /// counted rather than silently dropping the request's trace.
    #[test]
    fn an_empty_pool_is_counted_and_never_blocks() {
        let cal = Calibration::measure_for(std::time::Duration::from_millis(50));
        let pool = pool_for(cal, 1, 1, 0);
        let held: Vec<_> = (0..2).map(|_| pool.checkout()).collect();
        assert!(
            held.iter().all(|m| m.is_some()),
            "the pool holds max_hot + admission"
        );
        assert_eq!(pool.exhausted_checkouts(), 0);
        assert!(pool.checkout().is_none(), "a dry pool hands back None");
        assert_eq!(pool.exhausted_checkouts(), 1, "and counts it");
        drop(held);
        assert!(pool.checkout().is_some(), "a returned guard is reusable");
    }

    /// **The coverage property, on a trace built by hand.** Last mark minus
    /// first must be the whole span, and it must not be the sum of the
    /// corrected spans — which is short by one mark's cost per span, and would
    /// be the easy thing to publish by mistake.
    #[test]
    fn the_span_is_last_minus_first_and_not_the_sum_of_the_parts() {
        let cal = Calibration::measure_for(std::time::Duration::from_millis(50));
        let pool = pool_for(cal, 4, 0, 0);
        let mut m = pool.checkout();
        begin(&mut m);
        for _ in 0..8 {
            std::hint::black_box(spin(2_000));
            mark(&mut m, Mark::GuestReturned);
        }
        stop(&mut m, Mark::CallerReceived);
        let t = read(m.as_ref().unwrap());
        assert_eq!(t.spans.len(), 9);
        let summed: f64 = t.spans.iter().map(|(_, ns)| ns).sum();
        assert!(t.span_ns > 0.0, "a span of {} ns", t.span_ns);
        assert!(
            t.span_ns > summed,
            "the raw span ({:.1} ns) must exceed the sum of overhead-corrected spans \
             ({summed:.1} ns) by about one mark each",
            t.span_ns
        );
        // …and by roughly the right amount: nine marks' worth, within a
        // generous factor, so a change that stopped subtracting overhead at
        // all would fail here rather than pass quietly.
        let per_mark = cal.ticks_to_nanos(cal.mark_overhead_ticks());
        let gap = t.span_ns - summed;
        assert!(
            gap > per_mark * 4.0 && gap < per_mark * 20.0,
            "the gap is {gap:.1} ns against {} marks at {per_mark:.1} ns each",
            t.spans.len()
        );
    }

    fn spin(n: u64) -> u64 {
        let mut x = 0u64;
        for i in 0..n {
            x = x.wrapping_add(i).wrapping_mul(2_654_435_761);
        }
        x
    }
}
