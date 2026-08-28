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

    // --- the enqueue, lock-free structure -----------------------------------
    /// One `SeqCst` read-modify-write on a cacheline every producer touches.
    PushRegistered,
    /// The `closed` check.
    ///
    /// **The producer's last mark on this path.** Everything after it — the
    /// ring's compare-and-swap, the `SeqCst` decrement and the notify — is
    /// inside the span that closes at [`Mark::RingTaken`], because a ring's
    /// producer has no instant after the push at which it still knows which
    /// slot it wrote. A shed is decided in there too: `push` hands the job
    /// straight back when the ring is full, and then the span closes at
    /// [`Mark::ShedDecided`] instead.
    ClosedChecked,

    // --- the queue ---------------------------------------------------------
    /// **The queue wait**: the job sitting in its lane until an execution
    /// thread owns it. Workers being busy is the thing that stretches it.
    ///
    /// What it contains depends on whether a worker had to be woken. If one
    /// did, [`Mark::WorkerWoken`] splits the futex round trip out in front of
    /// this and what remains here is the consumer's own lane acquisition and
    /// pop — tens of nanoseconds. If no worker had to be woken there is no
    /// round trip, `WorkerWoken` is absent, and this is the whole wait. Adding
    /// the two rows is therefore meaningful and subtracting them is not.
    ///
    /// **Mutex structures only.** A ring's wait is [`Mark::RingTaken`], and it
    /// is not the same quantity.
    JobTaken,
    /// **The lock-free ring's push and its wait, together.**
    ///
    /// Its own label rather than a second observation of [`Mark::JobTaken`],
    /// for the reason [`Mark::WakeRingTaken`] is its own label on the wake
    /// lane: it is not the same quantity. It contains the ring's
    /// compare-and-swap and its notify as well as the wait for a worker, where
    /// a mutex lane has those in `lane.lock.hold` and `wake.worker.woke`
    /// beside `queue.wait` rather than inside it.
    ///
    /// The merge is a property of the ring, not of the instrument. The
    /// producer's `pushing` guard has to be released *after* the push, and
    /// there is no instant between the push and the release at which the
    /// producer still owns the job to mark through — so a mark taken there
    /// races the consumer, and a mark that arrives out of order does not go
    /// missing, it renames the pair it falls between. One honest span beats
    /// two that sometimes swap names.
    ///
    /// **[`crate::cell::QueueStructure::LockFree`] only.**
    RingTaken,
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
    /// Releasing the sandbox: wait and hold together. A **pair**, and it stays
    /// one deliberately — see "Two spans that were specified and are
    /// deliberately not here" above for why a third mark inside
    /// `impl Drop for CallGuard` costs more than the split is worth.
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
    /// `SyncSender::send` blocks on a full channel, and the sender is an
    /// execution-pool thread — so a caller that stops draining stops the cell,
    /// and a caller that drains slowly holds a worker. This span was invisible
    /// to both ends until the trace stopped having a single owner: the buffer
    /// travels *inside* the result, so the thread doing the sending has
    /// nothing of its own left to mark with.
    ///
    /// **Present only when the channel really was full**, because `respond`
    /// tries a non-blocking send first and only marks when that comes back
    /// `Full`. The row's *presence* is therefore the finding, and its count is
    /// how often the caller was too slow to keep up — where an unconditional
    /// mark produced a row on every reply whose value was usually this
    /// thread's own wake-up of a parked receiver, which is the answerer's cost
    /// rather than the request's.
    ///
    /// That is also what makes the reading safe to take. The trace travels
    /// inside the result and the collector reads it immediately on `recv`, with
    /// no guest call in between the way the queue handoffs have — so an
    /// unconditional mark here races the reader and loses the slow sends
    /// first. A blocked `send` returns when the receiver takes the *head* of
    /// the channel, and this result is at the tail behind everything already
    /// queued, so the reader cannot be about to read this trace.
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
    /// Timestamp when a host future completion reaches the cell.
    /// The wake worker later attaches this reading to its request trace.
    CompletionArrived,
    /// **Waiting for the global future table on the completion path.**
    ///
    /// The reactor's own contribution to the lock `FutureTable` calls "the
    /// binding one" — 91–99% of everything the execution threads waited on —
    /// measured from the other side for the first time.
    FutureCompleteAcquired,
    /// Holding it: a lookup, the state and result writes, a `pending` removal,
    /// a per-dispatch decrement and a `ready` push. Comfortably past a mark,
    /// which is what makes this a triple.
    FutureCompleteReleased,
    /// The sampling decision and building the carrier the readings travel on —
    /// **the instrument's own allocation, inside a span rather than hidden in
    /// one**. One `Box` per sampled wake, on the reactor thread.
    WakeIndexWanted,
    /// **The wake router's index**: wait and hold, as a **pair**. The hold is
    /// one hash, one lookup and a `Weak::upgrade`, at about what a mark costs.
    /// Every completion in the cell passes through this one mutex, and it
    /// appears in no row of `Cell::lock_stats`.
    WakeIndexReleased,
    /// The `wakes` counter and building the `WakeItem`, then choosing a lane.
    /// **Mutex structures only.**
    WakeLaneLockWanted,
    /// **The wake lane's lock, waited for on the reactor thread.** A wake is
    /// never shed, so unlike a dispatch it cannot leave here empty-handed — it
    /// waits however long the lane's consumers make it wait. **Mutex
    /// structures only.**
    WakeLaneLockAcquired,
    /// End of the mutex-backed wake queue critical section.
    WakeLaneLockReleased,
    /// **From the item being visible in its lane to a worker being awake and
    /// holding the lane again** — the futex round trip, on the request's clock.
    ///
    /// Read by the **consumer**, on its first instruction out of
    /// `Condvar::wait`, and carried into whichever item it then pops.
    ///
    /// **On both job kinds.** A dispatch parks a worker and wakes one exactly
    /// as a wake does, and the reading is taken in the same place either way —
    /// so this is one row over both rather than a wake-only row beside a
    /// dispatch-only one. What differs is only how the reading travels: a wake
    /// has no trace on the pushing thread and rides a
    /// [`WakeStamps`] carrier, where a dispatch carries its own trace and the
    /// reading is filed straight into it.
    ///
    /// It was briefly the producer's `notify_one` *returning* instead, on both
    /// paths, and that was wrong twice over. It raced: the thread the call
    /// wakes starts running while the producer is still returning from the
    /// syscall, and can pop, reclaim the trace and file first — so the reading
    /// lands too late. Sometimes it is simply dropped, and sometimes it lands
    /// after its neighbour, which is worse: the trace is one timeline sorted by
    /// timestamp and each span is named after the mark that *ends* it, so a
    /// late mark does not go missing, it renames the pair it falls between.
    /// And the readings it lost were the *slow* notifies, which made the row a
    /// distribution truncated at exactly the tail it existed to show: 80%
    /// coverage on the wake lane, a p50 running short, a p99 of nothing in
    /// particular.
    ///
    /// Reading it on the consumer removes the race outright — the thread that
    /// takes the reading is the thread that files it — and it measures the
    /// better quantity. The producer's syscall duration is what the
    /// *submitter* pays; it overlaps the request's timeline without gating it.
    /// What gates the request is how long until a worker is actually running,
    /// which is this.
    ///
    /// **Absent when no worker had to be woken**, which is an honest zero and
    /// not a lost observation: a busy cell's workers are already looping in
    /// `pop` and never reach the condvar, and then the whole wait is
    /// [`Mark::JobTaken`], [`Mark::RingTaken`], [`Mark::WakeTaken`] or
    /// [`Mark::WakeRingTaken`].
    /// Also absent when the reading is refused as not belonging to the item it
    /// would have been filed on — see [`woke_belongs`] and [`Mark::caveat`].
    WorkerWoken,
    /// A worker takes a queued wake from a mutex-backed lane.
    WakeTaken,
    /// **The lock-free ring's push and its wait, together.**
    ///
    /// Its own label rather than a second observation of [`Mark::WakeTaken`],
    /// because it is not the same quantity: it contains the ring's push and its
    /// notify as well as the wait for a consumer. Under a mutex lane those two
    /// are `wake.lane.lock.hold` and `wake.notify` and they are separate rows;
    /// here they are not, and the label says which it is rather than letting a
    /// column compare a lane wait against a ring's compare-and-swap.
    ///
    /// The merge is a property of the ring, not of the instrument: the
    /// producer's `pushing` guard has to be released *after* the push, the item
    /// is what carries the readings, and there is no point between the push and
    /// the release at which the producer still knows which slot it wrote.
    /// **[`crate::cell::QueueStructure::LockFree`] only.**
    WakeRingTaken,
    /// The pop's remainder, the `catch_unwind` and `Weak::upgrade` in
    /// `exec_loop`, and the reclaim of the parked request's trace.
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
    /// **The pause ended, and what ended it was not measured on this request's
    /// behalf.**
    ///
    /// Taken only by a *stale* wake — one whose dispatch had already finished
    /// when a worker got to it — at the instant it finds ready work and
    /// commits to running a slice. See [`crate::metrics::Metrics::stale_wakes`].
    ///
    /// A wake carries no future id: it is a doorbell, and `take_ready`
    /// collects whatever the instance has ready. So a stale doorbell answered
    /// during someone else's request delivers *that* request's completion, and
    /// the slice which follows is genuinely theirs — their guest time, their
    /// CPU, their response. It has to be measured. What cannot be measured is
    /// the road the doorbell took to get here: the lane wait, the futex, the
    /// completion's own arrival are all readings from a request that has
    /// already gone home, and [`WakeStamps::file`] refuses them.
    ///
    /// Which leaves a stretch with nothing to name it, and that is what this
    /// is for. Span labels are **positional** — a span takes the name of the
    /// mark that ends it — so without a mark here the request's whole pause
    /// would be absorbed into whichever mark landed next and reported under
    /// *its* name: a multi-second park filed as `active.budget.wanted`. The
    /// span closing here says what is actually known, which is that the
    /// request was paused until this instant and the reason it stopped being
    /// paused belongs to somebody else's accounting.
    ///
    /// It therefore sits where [`Mark::CompletionArrived`] sits on the ordinary
    /// path and carries the same left edge, [`Mark::SliceEnded`] — but under a
    /// different name, because it is a different quantity: this one includes
    /// the foreign wake's own overhead, and a reader adding it to `park.wait`
    /// would be adding two differently-sourced numbers.
    WakeAdopted,
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
    /// The shed counter, before the module table is reached.
    ShedMetersLockWanted,
    /// **Waiting for the module table's read lock, on the shed path.**
    ///
    /// Refusing is not free, and this is the part of it that surprises:
    /// attributing the shed to the tenant needs `Meters`, which lives behind the
    /// module table — a lock a shed request has otherwise never touched,
    /// because it is refused in `submit_with_marks` and never reaches
    /// `run_dispatch`'s own read of it. It is its own label rather than
    /// `modules.read.*` for exactly that reason: the two are the same lock
    /// under different contention and at different points in a request, and
    /// one row over both could not say which side was waiting.
    ShedMetersLockAcquired,
    /// End of updating dispatch-shedding counters.
    ShedMetersLockReleased,
    /// No such module. Refused **after** queueing and being given a worker.
    ModuleMissing,
    /// Building or finding a sandbox failed. Refused after the most expensive
    /// part of setup was already paid for — possibly after a full activation.
    AcquireFailed,
    /// Label for a single-writer admission refusal.
    SingleWriterRefused,
    /// The sandbox refused entry: busy, or tombstoned and refusing everything.
    EnterRefused,
    /// The meters and counters a hard kill bumps, before the breaker.
    BreakerKillWanted,
    /// The hang-breaker lock was acquired to record a hard budget kill.
    BreakerKillAcquired,
    /// Holding it: a `BreakerKey::clone` — a `ModuleId`'s two `String`s plus
    /// the route — allocated *inside* the hold before the entry is hashed, so
    /// this is well past what a mark costs and separates cleanly from the
    /// wait.
    BreakerKillReleased,
    /// Start of recording the tombstone and related counters.
    TombstoneRingWanted,
    /// **Waiting for the tombstone ring.**
    TombstoneRingAcquired,
    /// End of recording the tombstone and related counters.
    TombstoneRingReleased,

    // --- the caller's side again --------------------------------------------
    /// The reply crossing the channel and the collector being scheduled, plus
    /// any time the cell was blocked on a full channel. **The last mark of the
    /// request**; `stop()` is taken here.
    CallerReceived,

    /// Start of post-response cleanup.
    FinishBegan,
    /// Nothing; the left edge of the in-flight record's removal.
    ActiveRemoveWanted,
    /// Taking the request out of `Cell::active`: wait and hold. A **pair** —
    /// the hold is a hash and a `HashMap::remove` that *moves* the entry out,
    /// with nothing cloned inside it, so it sits at about what one mark costs.
    /// Note this is the fifth acquisition of the same lock on the same key.
    ActiveRemoveReleased,
    /// Nothing; the left edge of the future table.
    EndDispatchWanted,
    /// **Waiting for the global future table**, to reap the dispatch's
    /// handles.
    EndDispatchAcquired,
    /// Holding it: a `BTreeSet` clone of the `wait_until` set, a `Vec` of
    /// every pending id, a filtered pass over both and a removal per doomed
    /// entry — two allocations and three structure walks, comfortably past a
    /// mark.
    EndDispatchReleased,
    /// Disposition selection and, on eviction, `Cell::forget`.
    ReleaseInstanceWanted,
    /// **Waiting for the global future table**, to release the instance.
    ReleaseInstanceAcquired,
    /// Holding it — **and this is the most interesting figure in the tail.**
    /// `FutureTable::release_instance` does `entries.retain(|_, e| e.owner !=
    /// owner)`, which walks **every entry in the whole cell's future table**,
    /// not just this instance's. Its cost is therefore a function of what
    /// every *other* instance is doing, and it is paid by every dispatch that
    /// ends. There is an ownership index right beside it (`owned`) that is
    /// removed in the line above and would give the ids directly.
    ReleaseInstanceReleased,
    /// Nothing; the left edge of the return to the pool.
    PoolReleaseWanted,
    /// **Putting the sandbox back.** Not a lock pair: what happens in here is
    /// the disposition's, and the three are not comparable. `Warm` is a park
    /// and a `Vec` push under one mutex; `Reset` runs a whole
    /// `Slot::reset` — the instance is re-sealed — *before* the mutex;
    /// `Discard` hands the memory back to the kernel. One row over all three
    /// is deliberate: the question this answers is "how long does the worker
    /// spend not taking the next request", and the disposition is a property
    /// of the app rather than of the moment.
    PoolReleaseReleased,
    /// The discarded-state check and a second `Cell::forget` if it fires.
    FinishEnded,
    /// The job's own unwind, clearing the in-flight slot, and
    /// `WorkQueue::job_done` releasing the lane's load. **The last mark of the
    /// tail**, and the point at which the worker can take another request.
    JobDone,
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
    Mark::PushRegistered,
    Mark::ClosedChecked,
    Mark::JobTaken,
    Mark::RingTaken,
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
    Mark::CompletionArrived,
    Mark::FutureCompleteAcquired,
    Mark::FutureCompleteReleased,
    Mark::WakeIndexWanted,
    Mark::WakeIndexReleased,
    Mark::WakeLaneLockWanted,
    Mark::WakeLaneLockAcquired,
    Mark::WakeLaneLockReleased,
    Mark::WorkerWoken,
    Mark::WakeTaken,
    Mark::WakeRingTaken,
    Mark::WakeStarted,
    Mark::WakeResidentsLockWanted,
    Mark::WakeResidentsLockAcquired,
    Mark::WakeResidentsLockReleased,
    Mark::TakeReadyWanted,
    Mark::TakeReadyAcquired,
    Mark::TakeReadyReleased,
    Mark::WakeAdopted,
    Mark::ActiveBudgetWanted,
    Mark::ActiveBudgetReleased,
    Mark::BreakerRefused,
    Mark::ShedDecided,
    Mark::ShedMetersLockWanted,
    Mark::ShedMetersLockAcquired,
    Mark::ShedMetersLockReleased,
    Mark::ModuleMissing,
    Mark::AcquireFailed,
    Mark::SingleWriterRefused,
    Mark::EnterRefused,
    Mark::BreakerKillWanted,
    Mark::BreakerKillAcquired,
    Mark::BreakerKillReleased,
    Mark::TombstoneRingWanted,
    Mark::TombstoneRingAcquired,
    Mark::TombstoneRingReleased,
    Mark::CallerReceived,
    Mark::FinishBegan,
    Mark::ActiveRemoveWanted,
    Mark::ActiveRemoveReleased,
    Mark::EndDispatchWanted,
    Mark::EndDispatchAcquired,
    Mark::EndDispatchReleased,
    Mark::ReleaseInstanceWanted,
    Mark::ReleaseInstanceAcquired,
    Mark::ReleaseInstanceReleased,
    Mark::PoolReleaseWanted,
    Mark::PoolReleaseReleased,
    Mark::FinishEnded,
    Mark::JobDone,
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
            PushRegistered => "ring.pushing.enter",
            ClosedChecked => "ring.closed.check",
            JobTaken => "queue.wait",
            RingTaken => "ring.wait",
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
            CompletionArrived => "park.wait",
            FutureCompleteAcquired => "futures.complete.wait",
            FutureCompleteReleased => "futures.complete.hold",
            WakeIndexWanted => "wake.index.wanted",
            WakeIndexReleased => "wake.index",
            WakeLaneLockWanted => "wake.lane.lock.wanted",
            WakeLaneLockAcquired => "wake.lane.lock.wait",
            WakeLaneLockReleased => "wake.lane.lock.hold",
            WorkerWoken => "wake.worker.woke",
            WakeTaken => "wake.queue.wait",
            WakeRingTaken => "wake.ring.wait",
            WakeStarted => "wake.pop",
            WakeResidentsLockWanted => "wake.residents.wanted",
            WakeResidentsLockAcquired => "wake.residents.wait",
            WakeResidentsLockReleased => "wake.residents.hold",
            TakeReadyWanted => "futures.take.wanted",
            TakeReadyAcquired => "futures.take.wait",
            TakeReadyReleased => "futures.take.hold",
            WakeAdopted => "park.wait.unattributed",
            ActiveBudgetWanted => "active.budget.wanted",
            ActiveBudgetReleased => "active.budget",
            BreakerRefused => "refused.breaker",
            ShedDecided => "refused.shed",
            ShedMetersLockWanted => "shed.meters.wanted",
            ShedMetersLockAcquired => "shed.meters.wait",
            ShedMetersLockReleased => "shed.meters.hold",
            ModuleMissing => "refused.no_module",
            AcquireFailed => "refused.acquire",
            SingleWriterRefused => "refused.single_writer",
            EnterRefused => "refused.enter",
            BreakerKillWanted => "breaker.kill.wanted",
            BreakerKillAcquired => "breaker.kill.wait",
            BreakerKillReleased => "breaker.kill.hold",
            TombstoneRingWanted => "tombstone.ring.wanted",
            TombstoneRingAcquired => "tombstone.ring.wait",
            TombstoneRingReleased => "tombstone.ring.hold",
            ReplyHandedOff => "reply.handoff",
            SendReturned => "reply.send.blocked",
            CallerReceived => "reply.delivery",
            FinishBegan => "tail.finish.begin",
            ActiveRemoveWanted => "tail.active.remove.wanted",
            ActiveRemoveReleased => "tail.active.remove",
            EndDispatchWanted => "tail.futures.end.wanted",
            EndDispatchAcquired => "tail.futures.end.wait",
            EndDispatchReleased => "tail.futures.end.hold",
            ReleaseInstanceWanted => "tail.dispose.decide",
            ReleaseInstanceAcquired => "tail.futures.release.wait",
            ReleaseInstanceReleased => "tail.futures.release.hold",
            PoolReleaseWanted => "tail.pool.release.wanted",
            PoolReleaseReleased => "tail.pool.release",
            FinishEnded => "tail.finish.end",
            JobDone => "tail.job.done",
        }
    }

    /// **What is wrong with this row that its numbers cannot show.**
    ///
    /// `None` for almost every mark, and that is the point of it existing: a
    /// span whose population is not the whole of what it claims to measure has
    /// to say so *in the report*, beside its own numbers, or a reader quotes
    /// the p99 of a distribution that was truncated before they saw it.
    ///
    /// The per-row sample count already makes the shortfall visible to someone
    /// who compares it against a neighbour. This says what the shortfall is.
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Mark::WorkerWoken => Some(
                "only an item that had to wake a *parked* worker has this span, and that is \
                 true of dispatches and wakes alike. A busy cell's workers are already looping \
                 in `pop` and never reach the condvar, so they contribute nothing here and \
                 their whole wait is `queue.wait`, `wake.queue.wait` or `wake.ring.wait`. The \
                 shortfall against those rows' counts is the number of items taken by a worker \
                 that was already awake — a fact about the load rather than a gap in the \
                 instrument, and it does not bias what is here. A second, smaller shortfall is \
                 the instrument's own: under `lock-free` the reading can be taken before the \
                 item it ends up on was even pushed, because the rings are not covered by the \
                 park mutex and a worker can leave with something that arrived after it woke. \
                 Those readings are refused rather than filed — see `marks::woke_belongs` — so \
                 they are missing rows and never wrong ones. A mutex lane cannot do this: its \
                 consumer holds the lane lock from waking to popping, so nothing can be pushed \
                 in between",
            ),
            _ => None,
        }
    }

    /// Which queue structures this mark exists under.
    ///
    /// **Absent is not zero.** A `Shared` run has no `ring.wait` observations
    /// and a `LockFree` run has no `lane.lock.wait` observations, and the
    /// report has to be able to say which of those it is looking at without
    /// inferring it from a missing row.
    pub fn structural(self) -> Structural {
        use Mark::*;
        match self {
            LaneLockWanted | LaneLockAcquired | LaneLockReleased | JobTaken => {
                Structural::MutexQueue
            }
            PushRegistered | ClosedChecked | RingTaken => Structural::LockFreeQueue,
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
            | CompletionArrived
            | FutureCompleteAcquired
            | FutureCompleteReleased
            | WakeIndexWanted
            | WakeIndexReleased
            | WakeStarted
            | WakeResidentsLockWanted
            | WakeResidentsLockAcquired
            | WakeResidentsLockReleased
            | TakeReadyWanted
            | TakeReadyAcquired
            | TakeReadyReleased
            | WakeAdopted
            | ActiveBudgetWanted
            | ActiveBudgetReleased => Structural::ParkingRequest,
            WakeLaneLockWanted | WakeLaneLockAcquired | WakeLaneLockReleased | WakeTaken => {
                Structural::ParkingMutexQueue
            }
            WakeRingTaken => Structural::ParkingLockFreeQueue,
            SendReturned => Structural::BlockedReply,
            WorkerWoken => Structural::WokenWorker,
            BreakerRefused | ShedDecided | ModuleMissing | AcquireFailed | SingleWriterRefused
            | EnterRefused => Structural::RefusedRequest,
            ShedMetersLockWanted | ShedMetersLockAcquired | ShedMetersLockReleased => {
                Structural::ShedRequest
            }
            BreakerKillWanted | BreakerKillAcquired | BreakerKillReleased => {
                Structural::HardKilledRequest
            }
            TombstoneRingWanted | TombstoneRingAcquired | TombstoneRingReleased => {
                Structural::TombstonedRequest
            }
            FinishBegan
            | ActiveRemoveWanted
            | ActiveRemoveReleased
            | EndDispatchWanted
            | EndDispatchAcquired
            | EndDispatchReleased
            | ReleaseInstanceWanted
            | ReleaseInstanceAcquired
            | ReleaseInstanceReleased
            | PoolReleaseWanted
            | PoolReleaseReleased
            | FinishEnded
            | JobDone => Structural::CellTail,
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
    /// A reply that found the caller's channel full, so the answering worker
    /// blocked in `send` until the caller drained.
    ///
    /// Like [`Structural::WokenWorker`], a property of the load rather than of
    /// the request — here of the *caller's* load, which is the point: a
    /// non-empty count is the finding, and it says the harness could not keep
    /// up with the cell.
    BlockedReply,
    /// An item that found no worker running and had to wake one out of
    /// `Condvar::wait`.
    ///
    /// **A property of the load, not of the request.** The same guest at higher
    /// concurrency reaches it less often, because a busy cell's workers never
    /// park — so unlike every other class here, whether a request is in this
    /// one is decided by what the rest of the cell happened to be doing. It is
    /// a class rather than a caveat on [`Structural::Always`] because a row
    /// that is legitimately absent from most requests must not be described by
    /// `absence()` as one that should have been present under every
    /// configuration.
    ///
    /// Both job kinds are in it: a dispatch parks and wakes a worker exactly as
    /// a wake does.
    WokenWorker,
    /// A request that paused and was resumed. Every span in this class repeats
    /// once per pause, and the ones inside `run_wake`'s own loop can repeat
    /// several times per pause.
    ParkingRequest,
    /// A request that paused, in a cell whose lanes are mutexes. Two
    /// conditions rather than one, and it needs both: under `lock-free` the
    /// wake lane has no mutex, and in a run where nothing pauses there is no
    /// wake at all — and a reader told only the first would go looking for a
    /// lane that was there the whole time.
    ParkingMutexQueue,
    /// A request that paused, in a cell whose lanes are rings.
    ParkingLockFreeQueue,
    /// A request that was refused rather than served.
    RefusedRequest,
    /// A request the queue shed at admission. Narrower than
    /// [`Structural::RefusedRequest`], which also covers the breaker, the
    /// 404, the 409s and the acquisition failure — none of which touch the
    /// module table on the way out.
    ShedRequest,
    /// The dispatch discards its instance after a trap or hard kill.
    TombstonedRequest,
    /// The dispatch reaches its hard-budget termination path.
    HardKilledRequest,
    /// **Not part of the request at all.** What the worker does after the
    /// reply has gone: on its own trace, into its own sink, rendered in its
    /// own table. These spans overlap the request in wall-clock time — the
    /// caller is processing its reply while the worker is still releasing the
    /// instance — so adding one of them to a request span produces a number
    /// that is not a duration of anything.
    ///
    /// A request that **parked** has no tail: it ends its slice and hands the
    /// worker back without going through `finish_and_release`, so a run of
    /// nothing but parking guests observes none of these.
    CellTail,
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
            Structural::BlockedReply => {
                "only a reply that found the caller's channel full reaches it: the answering \
                 worker tries a non-blocking send first, and a caller keeping up never makes \
                 it block"
            }
            Structural::WokenWorker => {
                "only an item that had to wake a parked worker reaches it: a busy cell's \
                 workers are already looping in `pop` and never reach the condvar"
            }
            Structural::ParkingRequest => "only a request that paused and resumed reaches it",
            Structural::ParkingMutexQueue => {
                "only a request that paused reaches it, and only under `shared` or \
                 `per-worker`: the wake lane is a ring under `lock-free`"
            }
            Structural::ParkingLockFreeQueue => {
                "only a request that paused reaches it, and only under `lock-free`: the wake \
                 lane is a mutex under `shared` and `per-worker`"
            }
            Structural::RefusedRequest => "only a refused request reaches it",
            Structural::ShedRequest => "only a request the queue shed at admission reaches it",
            Structural::TombstonedRequest => {
                "only a dispatch that trapped or was killed at its hard budget reaches it"
            }
            Structural::HardKilledRequest => {
                "only a dispatch killed at its hard budget reaches it: §6.4 keeps a clean kill \
                 and a trap away from the breaker"
            }
            Structural::CellTail => {
                "runs after the reply, on the cell's own trace, and is reported apart from the \
                 request it followed"
            }
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

/// A reading that was never taken. Zero cannot be a real `RDTSCP` value on a
/// machine that has been up for any time at all.
pub const NO_STAMP: Stamp = (0, 0);

/// File a reading taken earlier.
#[inline(always)]
pub fn mark_at(m: &Option<RequestMarks>, label: Mark, at: Stamp) {
    if let Some(m) = m {
        m.mark_at(label as u8, at);
    }
}

/// **Does a worker's wake-up reading belong to the item it is about to be
/// filed against?**
///
/// A worker reads the counter as it comes out of `Condvar::wait` and then goes
/// looking for something to do. Under a mutex lane those are one step: the
/// wait returns holding the lane lock and the pop happens before it is
/// released, so nothing can be enqueued in between and the answer is always
/// yes. Under a lock-free ring they are not one step. The rings are not
/// covered by the park mutex, so between the reading and the pop another
/// producer can push, another worker can take the item this one was woken for,
/// and this worker can leave holding something that did not exist when it
/// woke.
///
/// **Filing it anyway is worse than dropping it.** [`crate::trace::read`] sorts
/// by timestamp and names each span after the mark that *ends* it, so a
/// reading older than the item does not merely add a wrong row — it lands in
/// front of the item's own marks and renames the pair it falls between. A
/// reading that predates the enqueue can land ahead of the whole admission
/// sequence, where the span it closes is a stretch of a *previous* thread's
/// work.
///
/// So the reading is kept only when it postdates the item's own enqueue.
/// Equality is kept: the counter steps by about 22 ticks, so two events inside
/// one step are indistinguishable and refusing them would drop honest
/// readings.
///
/// `pushed` is [`NO_STAMP`] on an item nobody stamped, and then there is
/// nothing to check against and the reading is refused rather than assumed
/// good. An absent row is this report's existing way of saying it did not
/// observe something; a wrong one has no such convention.
#[inline(always)]
pub fn woke_belongs(woke: Stamp, pushed: Stamp) -> bool {
    pushed.0 != 0 && woke.0 >= pushed.0
}

/// One carried reading, writable by whichever thread reaches its event.
///
/// Two atomics rather than a plain pair because a `WakeStamps` is written by
/// **three** threads: a reactor completing the future, whichever thread pushes
/// the wake, and the consumer that pops it. Each writes different fields, so
/// there is nothing to synchronise beyond the store itself.
///
/// Zero is "not taken". A real `RDTSCP` on a machine that has been up for any
/// time at all is never zero, and [`WakeStamps::file`] skips a zero rather than
/// filing it — which matters, because a zero would be a timestamp older than
/// any trace and [`crate::trace::Trace::mark_at`] would count it as a stale
/// reading.
#[derive(Debug, Default)]
pub struct StampCell {
    t: std::sync::atomic::AtomicU64,
    core: std::sync::atomic::AtomicU32,
}

impl StampCell {
    /// Record a reading taken earlier.
    #[inline(always)]
    pub fn set(&self, at: Stamp) {
        use std::sync::atomic::Ordering;
        self.core.store(at.1, Ordering::Relaxed);
        self.t.store(at.0, Ordering::Release);
    }
    /// Read the counter and record it here, in one call.
    #[inline(always)]
    pub fn take(&self) {
        self.set(stamp());
    }
    /// Read the reading **and clear it**, so it is filed once however many
    /// times [`WakeStamps::file`] is called.
    ///
    /// That is what lets the worker file the same carrier twice — once when it
    /// picks the wake up and once when it is about to hand the request to
    /// `settle` — and pick up on the second pass whatever the producer had not
    /// finished writing on the first. See [`WakeStamps`].
    #[inline(always)]
    fn take_reading(&self) -> Stamp {
        use std::sync::atomic::Ordering;
        let t = self.t.swap(0, Ordering::AcqRel);
        (t, self.core.load(Ordering::Relaxed))
    }
    /// Read **without** clearing.
    ///
    /// Its own method rather than a flag on [`StampCell::take_reading`],
    /// because the two have opposite obligations: `take_reading` is for a
    /// caller that is about to file the reading and must not file it twice,
    /// and this is for a caller that only wants to compare against it and must
    /// leave it for the filer. Getting that backwards would drop a reading
    /// rather than a comparison.
    #[inline(always)]
    fn peek(&self) -> Stamp {
        use std::sync::atomic::Ordering;
        let t = self.t.load(Ordering::Acquire);
        (t, self.core.load(Ordering::Relaxed))
    }
}

/// **Everything the completion path measured, on its way to the worker that
/// can file it.**
///
/// A parked request's trace lives in `Cell::active`, and no thread on this path
/// can reach it — the reactor completing the future, whoever pushes the wake,
/// the worker that pops it. Taking `Cell::active` on the wake path in order to
/// measure the wake path is self-defeating, so each reading is taken where its
/// event happens and the carrier travels with the queued wake.
///
/// **Shared, not owned, and for the reason [`crate::trace`] exists.** Pushing
/// the wake is a handoff: `VecDeque::push_back` and the ring's `push` both take
/// the item by value, so the instant the push lands the producer has nothing
/// left to write into — and it still has to release the lane lock and wake a
/// worker, which is a futex call and the single largest thing on the dispatch
/// side of the same sequence. The dispatch path answers this with a
/// [`MarkWriter`] on the request's trace; a reactor thread has no trace to take
/// one from, so the carrier itself is shared instead and the producer keeps a
/// handle across the handoff.
///
/// A field left at zero is a reading that did not happen: the lane fields under
/// `lock-free`, and the whole completion prefix on a wake that came from
/// `Cell::queue_wake` rather than from a reactor.
///
/// **The producer can still be writing when the consumer starts reading**, so
/// [`WakeStamps::file`] clears each reading as it files it and the worker calls
/// it twice: once when it picks the wake up, and again just before it hands the
/// request to `settle`, several microseconds later with the guest's own slice
/// in between. Whatever had not landed on the first pass is picked up on the
/// second, and nothing is filed twice. That covers `lane_released`, which the
/// producer writes a few nanoseconds after the push.
///
/// It did **not** cover the futex wake, and the attempt is worth recording
/// because the fix was to measure a different thing rather than to synchronise.
/// Reading the counter after `Condvar::notify_one` returned put the producer in
/// a race it usually lost — the thread the call wakes starts running while the
/// producer is still returning from the syscall — and the readings it lost were
/// the *slow* notifies, so the row was truncated at exactly the tail it existed
/// to show: 80% coverage even with the second pass. The reading is now taken by
/// the **consumer**, on its first instruction out of `Condvar::wait`, which has
/// no race at all and measures what actually gates the request. See
/// [`Mark::WorkerWoken`].
#[derive(Debug, Default)]
pub struct WakeStamps {
    /// The outside world's answer reaching the cell.
    pub arrived: StampCell,
    /// The future table's lock, taken on the reactor thread.
    pub complete_acquired: StampCell,
    pub complete_released: StampCell,
    /// The wake router's index.
    pub index_wanted: StampCell,
    pub index_released: StampCell,
    /// The wake lane's mutex. Unset under `lock-free`.
    pub lane_wanted: StampCell,
    pub lane_acquired: StampCell,
    pub lane_released: StampCell,
    /// **Written by the consumer, not the producer**: the instant a worker came
    /// out of `Condvar::wait`.
    ///
    /// Unset when no worker was parked, and unset when the reading did not
    /// belong to this item — see [`WakeStamps::enqueued_at`] and
    /// [`woke_belongs`].
    pub woke: StampCell,
    /// The instant a consumer took the item off the lane.
    pub taken: StampCell,
    /// Whether the lane was a ring, which decides **which label** `taken` is
    /// filed under: under `lock-free` the span it closes also contains the
    /// ring's push and its notify, and that is not the same quantity as a mutex
    /// lane's wait.
    pub lock_free: std::sync::atomic::AtomicBool,
}

impl WakeStamps {
    /// **The last reading the producer took before it let this item go**, or
    /// [`NO_STAMP`] if it took none.
    ///
    /// The fields are written in the order a wake meets them, by threads that
    /// hand off in that order, so the newest of the pre-push readings is the
    /// closest thing to "when this item was enqueued" the carrier holds. That
    /// is what a consumer's `Condvar::wait` reading has to postdate in order to
    /// belong to this item — see [`woke_belongs`].
    ///
    /// Checked newest-first, so the common case is one load: a wake from a
    /// reactor has `index_released`, and one on a mutex lane has
    /// `lane_acquired`.
    ///
    /// **`lane_released` is deliberately not among them.** It is written
    /// *after* the push, on the far side of the handoff, and is precisely the
    /// reading `run_wake`'s second `file` pass exists to collect. Comparing
    /// against it would let a producer that has not reached it yet make a
    /// perfectly good wake-up reading look stale.
    #[inline]
    pub fn enqueued_at(&self) -> Stamp {
        for c in [
            &self.lane_acquired,
            &self.lane_wanted,
            &self.index_released,
            &self.index_wanted,
            &self.complete_released,
            &self.complete_acquired,
            &self.arrived,
        ] {
            let at = c.peek();
            if at.0 != 0 {
                return at;
            }
        }
        NO_STAMP
    }

    /// Note that this wake went through a ring rather than a lane mutex.
    #[inline(always)]
    pub fn ring(&self) {
        self.lock_free
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// File every reading that was taken, in the order the request met them.
    ///
    /// Order is for legibility only — [`crate::trace::read`] sorts by
    /// timestamp — but filing in event order is what makes a truncated trace
    /// lose the end of the pause rather than a hole in the middle of it.
    pub fn file(&self, m: &Option<RequestMarks>) {
        let f = |label: Mark, c: &StampCell| {
            let at = c.take_reading();
            if at.0 != 0 {
                mark_at(m, label, at);
            }
        };
        f(Mark::CompletionArrived, &self.arrived);
        f(Mark::FutureCompleteAcquired, &self.complete_acquired);
        f(Mark::FutureCompleteReleased, &self.complete_released);
        f(Mark::WakeIndexWanted, &self.index_wanted);
        f(Mark::WakeIndexReleased, &self.index_released);
        f(Mark::WakeLaneLockWanted, &self.lane_wanted);
        f(Mark::WakeLaneLockAcquired, &self.lane_acquired);
        f(Mark::WakeLaneLockReleased, &self.lane_released);
        f(Mark::WorkerWoken, &self.woke);
        let ring = self.lock_free.load(std::sync::atomic::Ordering::Relaxed);
        f(
            if ring {
                Mark::WakeRingTaken
            } else {
                Mark::WakeTaken
            },
            &self.taken,
        );
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

/// How many marks the cell's tail can take.
///
/// Fifteen are placed and the sequence has no loop in it, so 32 is double what
/// any tail can produce. It is a separate constant from [`MARKS_CAPACITY`]
/// because it is a separate buffer with a separate lifetime: one per
/// *execution thread* rather than one per request in flight, held for the few
/// microseconds between the reply and the worker taking its next job.
pub const TAIL_CAPACITY: usize = 32;

/// Where a worker sends the tail it has just finished measuring.
///
/// The tail cannot come home the way a request's trace does. That one travels
/// inside the reply, and the reply is exactly what has already left. So the
/// cell hands the reduced trace to a sink the harness installed, which
/// aggregates it into distributions the run reads at the end — the same shape
/// `Cell::lock_stats` uses, and for the same reason.
///
/// Called on an execution thread, once per traced tail, **after** the tail's
/// last mark, so nothing it does is inside a measured span. It is still that
/// thread's time: an implementation that blocks here holds a worker.
pub trait TailSink: Send + Sync + 'static {
    /// One finished tail, already reduced to spans.
    fn record(&self, tail: &Trace);
}

/// Buffers and sink for separate post-response cleanup measurements.
pub struct CellTails {
    /// One buffer per execution thread is the peak: a worker holds one from
    /// `finish_and_release` to the top of its next `pop`.
    pub pool: RequestMarksPool,
    pub sink: std::sync::Arc<dyn TailSink>,
}

impl std::fmt::Debug for CellTails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CellTails({} buffers)", self.pool.size())
    }
}

/// Build a pool for a cell's tails, sized to its execution threads.
///
/// The slack is not generosity. A worker holds its buffer across the whole
/// tail, so `exec_threads` is the true peak — but a dry pool is a tail that
/// goes untraced and the shortfall is counted in the same
/// `pool_exhausted` figure the request pool reports, where it would read as a
/// bias rather than as an off-by-one.
pub fn tail_pool_for(cal: Calibration, exec_threads: usize) -> RequestMarksPool {
    TracePool::new(cal, exec_threads + 2, TAIL_CAPACITY)
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

/// A finished request reduced to stage intervals and trace fault flags.
/// Pool-level sampling and ownership counters are recorded separately.
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
        // Both lanes, dispatch and wake. The wake lane's labels were the easy
        // ones to get wrong: under `lock-free` the ring's push cannot be
        // separated from the wait for a consumer, so its closing span contains
        // one more thing than the mutex lane's does — and a shared label there
        // would put a compare-and-swap into the same distribution as a mutex
        // wait, in a table read as a comparison between the two designs.
        let of = |c: Structural| -> Vec<Mark> {
            every_mark()
                .into_iter()
                .filter(|m| m.structural() == c)
                .collect()
        };
        for (m, f) in [
            (of(Structural::MutexQueue), of(Structural::LockFreeQueue)),
            (
                of(Structural::ParkingMutexQueue),
                of(Structural::ParkingLockFreeQueue),
            ),
        ] {
            assert!(!m.is_empty() && !f.is_empty());
            for a in &m {
                for b in &f {
                    assert_ne!(a.name(), b.name(), "{a:?} and {b:?} share a span name");
                }
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
    /// **A wake-up reading is kept only when the item it lands on was already
    /// queued**, and both halves are asserted.
    ///
    /// Without the second half this would pass on an implementation that
    /// refused every reading, and `wake.worker.woke` is a reading on every
    /// item whose worker had to be woken — which is most of them on a quiet
    /// cell.
    ///
    /// The refused case is the one that costs a row rather than corrupting
    /// one. A reading older than the item sorts *before* the item's own marks,
    /// and since a span is named after the mark that ends it, filing it would
    /// not add a wrong row — it would rename a right one.
    #[test]
    fn a_wake_up_reading_older_than_its_item_is_refused() {
        let pushed = (1_000_u64, 3_u32);
        assert!(
            woke_belongs((1_001, 3), pushed),
            "a worker woken after the push owns the reading"
        );
        assert!(
            woke_belongs(pushed, pushed),
            "one counter step is not a violation"
        );
        assert!(
            !woke_belongs((999, 3), pushed),
            "a reading from before the push cannot belong to the item pushed after it"
        );
        assert!(
            !woke_belongs((1_001, 3), NO_STAMP),
            "an item nobody stamped has nothing to check against, so the reading is refused \
             rather than assumed good"
        );
        assert!(
            !woke_belongs(NO_STAMP, pushed),
            "a reading nobody took is not a reading"
        );
    }

    /// **A carrier can say when its item was enqueued, and saying so does not
    /// spend the reading.**
    ///
    /// Two claims, and the second is the one that would fail silently. The
    /// readings this compares against are the same ones `WakeStamps::file` has
    /// to file later; if the comparison cleared them the guard would work
    /// perfectly and quietly delete `futures.complete.*` and `wake.index.*`
    /// from every traced pause.
    #[test]
    fn a_carrier_reports_its_enqueue_without_consuming_it() {
        let st = WakeStamps::default();
        assert_eq!(
            st.enqueued_at(),
            NO_STAMP,
            "an empty carrier has no enqueue to report"
        );

        // The trace is opened first and the readings are built on top of a real
        // counter value, because `Trace::mark_at` refuses anything older than
        // the trace's own origin — a synthetic `(10, 1)` is refused as stale
        // and the second half of this test would then pass for the wrong
        // reason.
        let pool = pool_for(
            Calibration::measure_for(std::time::Duration::from_millis(50)),
            1,
            0,
            0,
        );
        let m = pool.checkout();
        begin(&m);
        let t0 = stamp().0;

        // A wake raised inside the cell sets only `arrived`, and that is then
        // the whole of what it can be compared against.
        st.arrived.set((t0 + 10, 1));
        assert_eq!(st.enqueued_at(), (t0 + 10, 1));

        // A reactor's wake gets further, and the newest pre-push reading wins.
        st.complete_acquired.set((t0 + 20, 1));
        st.complete_released.set((t0 + 30, 1));
        st.index_wanted.set((t0 + 40, 1));
        st.index_released.set((t0 + 50, 1));
        assert_eq!(
            st.enqueued_at(),
            (t0 + 50, 1),
            "the newest reading before the push"
        );

        // On a mutex lane the lane's own readings are newer still.
        st.lane_wanted.set((t0 + 60, 1));
        st.lane_acquired.set((t0 + 70, 1));
        assert_eq!(st.enqueued_at(), (t0 + 70, 1));

        // `lane_released` is written *after* the push and must not count: a
        // producer that has not reached it yet would otherwise make a good
        // wake-up reading look stale.
        st.lane_released.set((t0 + 80, 1));
        assert_eq!(
            st.enqueued_at(),
            (t0 + 70, 1),
            "a post-push reading is not an enqueue"
        );

        // And nothing above was spent: every reading is still there to file.
        st.file(&m);
        let got: Vec<Mark> = read(m.as_ref().unwrap())
            .spans
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        for l in [
            Mark::CompletionArrived,
            Mark::FutureCompleteAcquired,
            Mark::FutureCompleteReleased,
            Mark::WakeIndexWanted,
            Mark::WakeIndexReleased,
            Mark::WakeLaneLockWanted,
            Mark::WakeLaneLockAcquired,
            Mark::WakeLaneLockReleased,
        ] {
            assert!(
                got.contains(&l),
                "`{}` was consumed by the comparison",
                l.name()
            );
        }
    }

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
