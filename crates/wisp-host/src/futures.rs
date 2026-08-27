//! Host-owned futures, completion routing and per-instance resource limits.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use wisp_abi::result::{ResultRecord};

use crate::InstanceId;

/// Reserved: never a valid future id. Returned by the import vtable when
/// allocation is refused, so a guest that ignores the failure gets a wake that
/// never arrives rather than a wake for someone else's future.
pub const INVALID_FUTURE: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCapError {
    pub limit: u32,
    pub outstanding: u32,
}

impl std::fmt::Display for PendingCapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "simultaneous pending futures cap exceeded: {} outstanding, limit {} (§11.1)",
            self.outstanding, self.limit
        )
    }
}

/// What a completion means for the owner's lifecycle tier. The table does not
/// know where instances live, so it reports and the cell decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeTarget {
    /// The owning instance exists and now has a deliverable result.
    Instance(InstanceId),
    /// The future had no owner any more (instance discarded, dispatch over).
    /// The result is dropped; the host reclaimed the handle on instance death,
    /// which is invariant 2 doing the cleanup (§5.4).
    Orphan,
    /// Already completed; a duplicate completion is a reactor bug, not a
    /// guest-visible event.
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Pending,
    Ready,
    Delivered,
}

#[derive(Debug)]
struct Entry {
    owner: InstanceId,
    dispatch: u64,
    import: &'static str,
    state: State,
    result: Option<ResultRecord>,
}

#[derive(Debug, Default)]
struct Owned {
    /// Registered and not yet completed.
    pending: BTreeSet<u64>,
    /// Completed and not yet handed to the guest, in completion order. Order
    /// between distinct futures is unspecified by contract (§5.4), but FIFO is
    /// the least surprising unspecified order.
    ready: VecDeque<u64>,
    /// Pending count per dispatch — the unit §11.1 caps.
    per_dispatch: HashMap<u64, u32>,
}

struct Inner {
    entries: HashMap<u64, Entry>,
    owned: HashMap<InstanceId, Owned>,
}

pub struct FutureTable {
    next: AtomicU64,
    /// Synchronized future records and owner indexes.
    inner: crate::lockstats::CountedMutex<Inner>,
}

impl Default for FutureTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FutureTable {
    /// What waiting for the future table cost. See `lockstats`.
    pub fn lock_stats(&self) -> crate::lockstats::LockSample {
        self.inner.stats().snapshot()
    }

    /// Create an empty host future table.
    pub fn new() -> Self {
        Self {
            // Start at 1: id 0 is reserved as INVALID_FUTURE.
            next: AtomicU64::new(1),
            inner: crate::lockstats::CountedMutex::new(Inner {
                entries: HashMap::new(),
                owned: HashMap::new(),
            }),
        }
    }

    /// Register a future for `owner` under `dispatch`.
    ///
    /// `limit` is the application's `pending_futures_per_dispatch` (§11.1). Refusal is
    /// a `Result`, not a panic or a silent success, because the guest must be
    /// able to observe it as `TrapCode::TooManyFutures`.
    pub fn alloc(
        &self,
        owner: InstanceId,
        dispatch: u64,
        import: &'static str,
        limit: u32,
    ) -> Result<u64, PendingCapError> {
        let mut g = self.inner.lock();
        let owned = g.owned.entry(owner).or_default();
        let outstanding = *owned.per_dispatch.get(&dispatch).unwrap_or(&0);
        if outstanding >= limit {
            return Err(PendingCapError { limit, outstanding });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        owned.pending.insert(id);
        *owned.per_dispatch.entry(dispatch).or_insert(0) += 1;
        g.entries.insert(
            id,
            Entry {
                owner,
                dispatch,
                import,
                state: State::Pending,
                result: None,
            },
        );
        Ok(id)
    }

    /// Register the dispatch's **implicit cancellation future** (§5.4).
    ///
    /// Separate from [`FutureTable::alloc`] for one reason, and it is a
    /// contract reason rather than a convenience: §11.1's
    /// `pending_futures_per_dispatch` caps the fan-out a *guest* can create,
    /// and this future is not the guest's. Charging it to the cap would
    /// silently shrink every tenant's declared limit by one and make the
    /// platform's own bookkeeping the thing that trips a tenant's trap.
    ///
    /// It cannot fail, for the same reason: there is no limit to refuse it
    /// against. That matters — a dispatch whose cancellation future failed to
    /// allocate would be a dispatch that can only ever be hard-killed, which
    /// is precisely the outcome §6.4 is trying to make rare.
    pub fn alloc_cancellation(&self, owner: InstanceId, dispatch: u64) -> u64 {
        self.alloc_cancellation_marked(owner, dispatch, &mut None)
    }

    /// [`FutureTable::alloc_cancellation`], marking the wait for the table's
    /// lock separately from the work done under it.
    ///
    /// **A triple, not a pair.** The hold is 60 ns and more — two map inserts
    /// and a set insert, any of which can grow — comfortably above the 20 ns a
    /// mark costs on this machine, so the wait and the hold are separable and
    /// worth separating. This is a global lock every dispatch takes.
    pub fn alloc_cancellation_marked(
        &self,
        owner: InstanceId,
        dispatch: u64,
        marks: &mut Option<crate::marks::RequestMarks>,
    ) -> u64 {
        let mut g = self.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::CancelAllocAcquired);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let owned = g.owned.entry(owner).or_default();
        owned.pending.insert(id);
        g.entries.insert(
            id,
            Entry {
                owner,
                dispatch,
                import: "ctx_cancel",
                state: State::Pending,
                result: None,
            },
        );
        id
    }

    /// A reactor thread completed a host future. Maps id → owning instance and
    /// makes the result deliverable; the caller enqueues the wake (§6.1).
    pub fn complete(&self, id: u64, result: ResultRecord) -> WakeTarget {
        self.complete_marked(id, result, false).target
    }

    /// [`FutureTable::complete`], reading the counter after the lock and
    /// handing back the dispatch the future belonged to.
    ///
    /// **Both extras are needed before the caller can decide anything.** The
    /// sampling predicate runs on a dispatch id, and the dispatch id is inside
    /// this table under this lock — so a reading of the wait for the lock has
    /// to be taken before the cell knows whether it wants one. `taking` is
    /// therefore "is this cell tracing at all", not "is this request sampled",
    /// and while it is true every completion pays one `RDTSCP`. At zero
    /// sampling it is false and the cost is a branch.
    ///
    /// **A triple**, with [`crate::marks::Mark::CompletionArrived`] as the left
    /// edge: the hold is a lookup, two writes, a `pending` removal, a
    /// per-dispatch decrement and a `ready` push, comfortably past a mark.
    pub fn complete_marked(&self, id: u64, result: ResultRecord, taking: bool) -> Completed {
        let mut g = self.inner.lock();
        let acquired = if taking {
            crate::marks::stamp()
        } else {
            crate::marks::NO_STAMP
        };
        let Some(e) = g.entries.get_mut(&id) else {
            return Completed {
                target: WakeTarget::Orphan,
                dispatch: 0,
                acquired,
            };
        };
        if e.state != State::Pending {
            return Completed {
                target: WakeTarget::Duplicate,
                dispatch: e.dispatch,
                acquired,
            };
        }
        e.state = State::Ready;
        e.result = Some(result);
        let owner = e.owner;
        let dispatch = e.dispatch;
        if let Some(o) = g.owned.get_mut(&owner) {
            o.pending.remove(&id);
            if let Some(n) = o.per_dispatch.get_mut(&dispatch) {
                *n = n.saturating_sub(1);
            }
            o.ready.push_back(id);
        }
        Completed {
            target: WakeTarget::Instance(owner),
            dispatch,
            acquired,
        }
    }

    /// Pull the next deliverable result for `owner`. The exec-pool thread calls
    /// this in a loop while it holds the instance's [`crate::CallGuard`], which
    /// is how a completion that lands mid-dispatch is delivered *after* the
    /// current call returns rather than reentrantly (§5.4).
    pub fn take_ready(&self, owner: InstanceId) -> Option<(u64, ResultRecord)> {
        self.take_ready_marked(owner, &mut None)
    }

    /// [`FutureTable::take_ready`], marking the wait for the table separately
    /// from the work done under it.
    ///
    /// **A triple.** The hold is 80 ns and more — a loop popping and removing
    /// across three structures — and it is taken on every resumed slice, so
    /// the wait and the hold are worth telling apart.
    pub fn take_ready_marked(
        &self,
        owner: InstanceId,
        marks: &mut Option<crate::marks::RequestMarks>,
    ) -> Option<(u64, ResultRecord)> {
        let mut g = self.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::TakeReadyAcquired);
        loop {
            let id = g.owned.get_mut(&owner)?.ready.pop_front()?;
            let Some(e) = g.entries.get_mut(&id) else {
                continue;
            };
            let Some(result) = e.result.take() else {
                continue;
            };
            e.state = State::Delivered;
            g.entries.remove(&id);
            return Some((id, result));
        }
    }

    pub fn has_ready(&self, owner: InstanceId) -> bool {
        self.inner
            .lock()
            .owned
            .get(&owner)
            .is_some_and(|o| !o.ready.is_empty())
    }




    pub fn pending_for_dispatch(&self, owner: InstanceId, dispatch: u64) -> u32 {
        let g = self.inner.lock();
        g.owned
            .get(&owner)
            .and_then(|o| o.per_dispatch.get(&dispatch))
            .copied()
            .unwrap_or(0)
    }

    pub fn pending_total(&self, owner: InstanceId) -> usize {
        self.inner
            .lock()
            .owned
            .get(&owner)
            .map_or(0, |o| o.pending.len())
    }

    /// Reclaim pending futures owned by the completed dispatch.
    pub fn end_dispatch(&self, owner: InstanceId, dispatch: u64) {
        self.end_dispatch_marked(owner, dispatch, &None)
    }

    /// Reclaim pending futures owned by the completed dispatch.
    pub fn end_dispatch_marked(
        &self,
        owner: InstanceId,
        dispatch: u64,
        marks: &Option<crate::marks::RequestMarks>,
    ) {
        let mut g = self.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::EndDispatchAcquired);
        let Some(o) = g.owned.get_mut(&owner) else {
            return;
        };
        o.per_dispatch.remove(&dispatch);
        let undelivered: Vec<u64> = o.pending.iter().chain(o.ready.iter()).copied().collect();
        let doomed: Vec<u64> = undelivered
            .into_iter()
            .filter(|id| g.entries.get(id).is_some_and(|e| e.dispatch == dispatch))
            .collect();
        for id in doomed {
            g.entries.remove(&id);
            if let Some(o) = g.owned.get_mut(&owner) {
                o.pending.remove(&id);
                o.ready.retain(|r| *r != id);
            }
        }
    }

    /// Instance death. Every handle the instance held is reclaimed here; this
    /// is the mechanical form of §2.5.8's "cleanup lives outside the sandbox
    /// lifetime".
    pub fn release_instance(&self, owner: InstanceId) {
        self.release_instance_marked(owner, &None)
    }

    /// Release instance futures and record lock timing.
    pub fn release_instance_marked(
        &self,
        owner: InstanceId,
        marks: &Option<crate::marks::RequestMarks>,
    ) {
        let mut g = self.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::ReleaseInstanceAcquired);
        g.owned.remove(&owner);
        g.entries.retain(|_, e| e.owner != owner);
    }





    /// Every live future id, ascending. For tests and the `wisp dev`
    /// inspector.
    pub fn live_future_ids(&self) -> Vec<u64> {
        let g = self.inner.lock();
        let mut v: Vec<u64> = g.entries.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Live futures the **guest** issued, in id order.
    ///
    /// Excludes the implicit cancellation future §5.4 gives every dispatch,
    /// which the guest did not ask for and (usually) never awaits. A test
    /// reasoning about "the futures this dispatch registered" wants this one;
    /// [`FutureTable::live_future_ids`] is the whole table, cancellation
    /// included.
    pub fn live_guest_future_ids(&self) -> Vec<u64> {
        let g = self.inner.lock();
        let mut v: Vec<u64> = g
            .entries
            .iter()
            .filter(|(_, e)| e.import != "ctx_cancel")
            .map(|(id, _)| *id)
            .collect();
        v.sort_unstable();
        v
    }


    pub fn live_futures(&self) -> usize {
        self.inner.lock().entries.len()
    }
}

/// What [`FutureTable::complete_marked`] found, plus what it measured.
#[derive(Debug, Clone, Copy)]
pub struct Completed {
    pub target: WakeTarget,
    /// The dispatch the future was registered against. `0` when the id was not
    /// in the table at all — which is also not a dispatch, so nothing that
    /// reads this can be confused by it.
    pub dispatch: u64,
    /// The counter, read immediately after the table's lock was acquired.
    /// [`crate::marks::NO_STAMP`] when the cell is not tracing.
    pub acquired: crate::marks::Stamp,
}


#[cfg(test)]
mod tests {
    use super::*;


    fn table() -> FutureTable {
        FutureTable::new()
    }

    #[test]
    fn ids_are_never_zero_and_never_reused() {
        let t = table();
        let i = InstanceId(1);
        let a = t.alloc(i, 1, "kv_get", 64).unwrap();
        let b = t.alloc(i, 1, "kv_get", 64).unwrap();
        assert_ne!(a, INVALID_FUTURE);
        assert_ne!(a, b);
    }

    /// Pending futures are capped per dispatch.
    #[test]
    fn pending_cap_is_enforced_per_dispatch() {
        let t = table();
        let i = InstanceId(1);
        let limit = 6;
        assert_eq!(limit, 6);
        for n in 0..limit {
            assert!(
                t.alloc(i, 7, "kv_get", limit).is_ok(),
                "alloc {n} refused early"
            );
        }
        let err = t.alloc(i, 7, "kv_get", limit).unwrap_err();
        assert_eq!(
            err,
            PendingCapError {
                limit: 6,
                outstanding: 6
            }
        );

        // A *different* dispatch on the same instance has its own budget: the
        // cap in §11.1 is per dispatch, not per instance.
        assert!(t.alloc(i, 8, "kv_get", limit).is_ok());
    }

    #[test]
    fn completing_a_future_frees_cap_headroom() {
        let t = table();
        let i = InstanceId(1);
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(t.alloc(i, 1, "kv_get", 6).unwrap());
        }
        assert!(t.alloc(i, 1, "kv_get", 6).is_err());
        t.complete(ids[0], ResultRecord::Ok(vec![1]));
        assert!(t.alloc(i, 1, "kv_get", 6).is_ok());
    }

    #[test]
    fn completion_routes_to_the_owner_and_is_idempotent() {
        let t = table();
        let a = InstanceId(1);
        let b = InstanceId(2);
        let fa = t.alloc(a, 1, "kv_get", 64).unwrap();
        let fb = t.alloc(b, 1, "kv_get", 64).unwrap();
        assert_eq!(
            t.complete(fa, ResultRecord::Ok(vec![1])),
            WakeTarget::Instance(a)
        );
        assert_eq!(
            t.complete(fb, ResultRecord::Ok(vec![2])),
            WakeTarget::Instance(b)
        );
        assert_eq!(
            t.complete(fa, ResultRecord::Ok(vec![9])),
            WakeTarget::Duplicate
        );
        assert_eq!(
            t.complete(9_999, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan
        );

        let (id, res) = t.take_ready(a).unwrap();
        assert_eq!(id, fa);
        assert!(matches!(res, ResultRecord::Ok(v) if v == vec![1]));
        assert!(t.take_ready(a).is_none());
    }






    #[test]
    fn ending_a_dispatch_reclaims_its_handles_and_preserves_other_dispatches() {
        let t = table();
        let i = InstanceId(1);
        let doomed = t.alloc(i, 1, "kv_get", 64).unwrap();
        let kept = t.alloc(i, 2, "queue_send", 64).unwrap();
        t.end_dispatch(i, 1);
        assert_eq!(t.pending_total(i), 1);
        assert_eq!(
            t.complete(doomed, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan
        );
        assert_eq!(
            t.complete(kept, ResultRecord::Ok(vec![])),
            WakeTarget::Instance(i)
        );
        t.take_ready(i);
    }

    /// Reclaim pending futures owned by the completed dispatch.
    #[test]
    fn a_completed_but_undelivered_result_does_not_outlive_its_dispatch() {
        let t = table();
        let i = InstanceId(1);
        let doomed = t.alloc(i, 1, "kv_get", 64).unwrap();
        let kept = t.alloc(i, 2, "queue_send", 64).unwrap();

        // Both resolve while the dispatch is still running, and neither is
        // collected: this is the race the hole was made of.
        assert_eq!(
            t.complete(doomed, ResultRecord::Ok(vec![1])),
            WakeTarget::Instance(i)
        );
        assert_eq!(
            t.complete(kept, ResultRecord::Ok(vec![2])),
            WakeTarget::Instance(i)
        );
        assert!(t.has_ready(i), "both results are queued and undelivered");

        t.end_dispatch(i, 1);

        let served = t.take_ready(i);
        assert_eq!(
            served,
            Some((kept, ResultRecord::Ok(vec![2]))),
            "a result remains live only for its owning dispatch"
        );
        assert!(
            !t.has_ready(i),
            "a result outlived the dispatch that awaited it"
        );
    }

    #[test]
    fn releasing_an_instance_reclaims_every_handle() {
        let t = table();
        let i = InstanceId(1);
        let f = t.alloc(i, 1, "kv_get", 64).unwrap();
        t.release_instance(i);
        assert_eq!(t.live_futures(), 0);
        assert_eq!(t.complete(f, ResultRecord::Ok(vec![])), WakeTarget::Orphan);
    }

}
