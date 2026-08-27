//! Labelled timestamp boundaries for dispatch and queue measurements.
//! Labels identify stage and queue boundaries. Available labels depend on the
//! request path and queue implementation; absent labels are not zero intervals.
use crate::tsc_marks::{Calibration, Marks, MarksPool, PooledMarks};

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

    // --- the enqueue, lock-free structure -----------------------------------
    /// One `SeqCst` read-modify-write on a cacheline every producer touches.
    PushRegistered,
    /// The `closed` check. **The ring's compare-and-swap is not marked**, for
    /// the same reason the lane's critical section is not: `ArrayQueue::push`
    /// takes the job by value and a consumer may pop it before the next
    /// instruction, so there is nothing left on this thread to mark. Its cost
    /// falls into `JobTaken`. That asymmetry with the mutex path is the honest
    /// statement — a lock-free queue has no lock wait to isolate.
    ClosedChecked,

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
    /// Nothing; the left edge of the live-sandbox table.
    ResidentsLockWanted,
    /// **Waiting for the live-sandbox table.**
    ResidentsLockAcquired,
    /// End of accessing the resident-instance table.
    ResidentsLockReleased,
    /// Whatever of the acquisition was not separately marked: the warm-pool
    /// pop, a hibernation wake, or a full activation.
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
    /// Building the result and the response.
    BreakerSuccessWanted,
    /// **Waiting for the hang breaker's lock a second time.**
    BreakerSuccessAcquired,
    /// End of recording success in the hang breaker.
    BreakerSuccessReleased,
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

    // --- the caller's side again --------------------------------------------
    /// The reply crossing the channel and the collector being scheduled, plus
    /// any time the cell was blocked on a full channel. **The last mark**;
    /// `stop()` is taken here.
    CallerReceived,
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
            PushRegistered => "ring.pushing.enter",
            ClosedChecked => "ring.closed.check",
            JobTaken => "queue.wait",
            DispatchStarted => "worker.start",
            ModulesLockWanted => "modules.read.wanted",
            ModulesLockAcquired => "modules.read.wait",
            ModulesLockReleased => "modules.read.hold",
            InstanceAcquireBegan => "acquire.begin",
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
            BreakerSuccessWanted => "breaker.success.wanted",
            BreakerSuccessAcquired => "breaker.success.wait",
            BreakerSuccessReleased => "breaker.success.hold",
            ResultReadWanted => "active.read.wanted",
            ResultReadReleased => "active.read",
            ResponderTakenWanted => "active.responder.wanted",
            ResponderTakenReleased => "active.responder",
            ReplyHandedOff => "reply.handoff",
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
            LaneLockWanted | LaneLockAcquired => Structural::MutexQueue,
            PushRegistered | ClosedChecked => Structural::LockFreeQueue,
            ResponseTableWanted | ResponseTableAcquired | ResponseTableReleased => {
                Structural::RespondingDispatch
            }
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
    /// A dispatch that completed with a response handle.
    RespondingDispatch,
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
            Structural::RespondingDispatch => {
                "only a dispatch that completed with a response handle reaches it"
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
pub type RequestMarks = PooledMarks<Mark>;

/// The pool a cell's requests check marks out of.
pub type RequestMarksPool = MarksPool<Mark>;

/// Take a mark, if this request is carrying any.
///
/// A free function rather than a method so the call site reads as one line at
/// each of the fifty-odd places it appears, and `#[inline(always)]` so an
/// unsampled request pays a predictable branch and no call.
#[inline(always)]
pub fn mark(m: &mut Option<RequestMarks>, label: Mark) {
    if let Some(m) = m.as_mut() {
        // Truncate rather than reallocate. `Marks::mark` debug-asserts on a
        // full buffer and would reallocate in release — inside the timed
        // region, on the dispatch path. Stopping at the cap costs this request
        // its tail and costs the run nothing.
        if m.len() < MARKS_CAPACITY {
            m.mark(label);
        }
    }
}

/// Begin a request's trace at [`Mark::CallerReleased`].
///
/// Clears whatever the pooled buffer last held. Named `begin` rather than
/// `start` so that the call site says which instant it is taking: this is the
/// request's origin, and the first *span* is the one ending at
/// [`Mark::CellEntered`].
#[inline(always)]
pub fn begin(m: &mut Option<RequestMarks>) {
    if let Some(m) = m.as_mut() {
        m.start();
    }
}

/// End a request's trace.
#[inline(always)]
pub fn stop(m: &mut Option<RequestMarks>, label: Mark) {
    if let Some(m) = m.as_mut() {
        if m.len() < MARKS_CAPACITY {
            m.stop(label);
        }
    }
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
    MarksPool::new(cal, max_hot + admission + slack, MARKS_CAPACITY)
}

// ---------------------------------------------------------------------------
// Reading a finished trace
// ---------------------------------------------------------------------------

/// One finished request, reduced to spans.
///
/// Produced on the collector thread, off the dispatch path, once per traced
/// request.
#[derive(Debug, Clone)]
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
    /// wake when a `Weak::upgrade` fails, and nothing else counts that.
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
pub fn read(m: &Marks<Mark>) -> Trace {
    let r = m.report();
    Trace {
        spans: r.intervals.iter().map(|iv| (iv.label, iv.nanos)).collect(),
        span_ns: m.span_nanos(),
        crossed_cores: r.crossed_cores,
        truncated: m.len() >= MARKS_CAPACITY,
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
        for s in [
            Structural::Always,
            Structural::MutexQueue,
            Structural::LockFreeQueue,
            Structural::RespondingDispatch,
        ] {
            assert!(!s.absence().is_empty());
        }
    }

    fn every_mark() -> Vec<Mark> {
        use Mark::*;
        vec![
            CallerReleased,
            CellEntered,
            BreakerLockWanted,
            BreakerLockAcquired,
            BreakerChecked,
            JobBuilt,
            PlacementDecided,
            LaneLockWanted,
            LaneLockAcquired,
            PushRegistered,
            ClosedChecked,
            JobTaken,
            DispatchStarted,
            ModulesLockWanted,
            ModulesLockAcquired,
            ModulesLockReleased,
            InstanceAcquireBegan,
            ResidentsLockWanted,
            ResidentsLockAcquired,
            ResidentsLockReleased,
            InstanceAcquireEnded,
            ActiveProbeWanted,
            ActiveProbeReleased,
            CancelAllocWanted,
            CancelAllocAcquired,
            CancelAllocReleased,
            ActiveInsertWanted,
            ActiveInsertReleased,
            SlotEnterWanted,
            SlotEnterAcquired,
            SlotEnterReleased,
            GuestEntered,
            GuestReturned,
            SlotReleaseWanted,
            SlotReleaseReleased,
            TenantRecordWanted,
            TenantRecordAcquired,
            TenantRecordReleased,
            ActiveUpdateWanted,
            ActiveUpdateAcquired,
            ActiveUpdateReleased,
            ResponseTableWanted,
            ResponseTableAcquired,
            ResponseTableReleased,
            WaitUntilCheckWanted,
            WaitUntilCheckAcquired,
            WaitUntilCheckReleased,
            BreakerSuccessWanted,
            BreakerSuccessAcquired,
            BreakerSuccessReleased,
            ResultReadWanted,
            ResultReadReleased,
            ResponderTakenWanted,
            ResponderTakenReleased,
            ReplyHandedOff,
            CallerReceived,
        ]
    }

    /// A trace that fills the buffer stops marking rather than reallocating.
    ///
    /// **The failure this prevents is invisible in the output.** `Marks::mark`
    /// pushes into a `Vec` sized once at pool construction; past its capacity
    /// it reallocates — inside the timed region, on the dispatch path — and
    /// the run that happens in reports a plausible number with a memcpy in the
    /// middle of it. A `debug_assert` catches that in a build the campaign
    /// never runs.
    #[test]
    fn a_full_buffer_truncates_instead_of_growing() {
        let cal = Calibration::measure_for(std::time::Duration::from_millis(50));
        let pool = pool_for(cal, 1, 0, 0);
        let mut m = pool.checkout();
        assert!(m.is_some(), "a pool of one hands out one");
        begin(&mut m);
        let cap = m.as_ref().unwrap().capacity();
        for _ in 0..cap * 2 {
            mark(&mut m, Mark::GuestReturned);
        }
        let g = m.as_ref().unwrap();
        assert!(g.len() <= cap, "the buffer grew: {} past {cap}", g.len());
        assert_eq!(
            g.len(),
            MARKS_CAPACITY,
            "it should have filled exactly to the cap"
        );
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
