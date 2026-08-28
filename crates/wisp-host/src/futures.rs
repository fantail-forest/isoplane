//! Host-owned futures, completion routing and per-instance resource limits.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

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
    /// Owner for completion routing.
    owner: Arc<OwnedFutures>,
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

/// Per-instance future ownership and pending counts.
#[derive(Debug)]
pub struct OwnedFutures {
    /// The instance this belongs to **now**. §6.8 wakes an instance under a
    /// new `InstanceId` (§8.2.4: portable memory, non-portable identity), and
    /// `complete` has to name the current one for the wake it returns.
    instance: AtomicU64,
    inner: Mutex<Owned>,
}

impl OwnedFutures {
    pub fn new(instance: InstanceId) -> Arc<OwnedFutures> {
        Arc::new(OwnedFutures {
            instance: AtomicU64::new(instance.0),
            inner: Mutex::new(Owned::default()),
        })
    }

    pub fn instance(&self) -> InstanceId {
        InstanceId(self.instance.load(Ordering::Acquire))
    }

}

pub struct FutureTable {
    next: AtomicU64,
    /// Future records keyed by allocated identifier.
    entries: scc::HashMap<u64, Entry>,
    /// Owner lookup keyed by instance identifier.
    by_instance: scc::HashMap<InstanceId, Arc<OwnedFutures>>,
}

impl Default for FutureTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FutureTable {
    /// **Nothing to report: there is no longer a lock here to wait for.**
    ///
    /// The table was one `CountedMutex` over two maps and this returned its
    /// sample. Both maps are `scc` now and the only remaining exclusion is one
    /// instance's own, which §7.2 keeps uncontended. `Cell::lock_stats` omits
    /// the row rather than printing this zero — a zero is a measurement and
    /// "there is nothing here to measure" is not one.
    pub fn lock_stats(&self) -> crate::lockstats::LockSample {
        crate::lockstats::LockSample::default()
    }

    /// This instance's future state, creating it on first use.
    fn owned_of(&self, owner: InstanceId) -> Arc<OwnedFutures> {
        // Cloned out and the bucket released before any caller locks it: the
        // one ordering rule in this file is **never acquire an `OwnedFutures`
        // lock while holding an `scc` bucket**. `take_ready` and
        // `end_dispatch` nest owned-then-entries; `complete` releases entries
        // before it touches owned. One direction only, so no cycle.
        self.by_instance
            .entry_sync(owner)
            .or_insert_with(|| OwnedFutures::new(owner))
            .get()
            .clone()
    }

    /// The same, without creating one.
    fn existing(&self, owner: InstanceId) -> Option<Arc<OwnedFutures>> {
        self.by_instance.read_sync(&owner, |_, a| a.clone())
    }

    /// Create an empty host future table.
    pub fn new() -> Self {
        Self {
            // Start at 1: id 0 is reserved as INVALID_FUTURE.
            next: AtomicU64::new(1),
            entries: scc::HashMap::new(),
            by_instance: scc::HashMap::new(),
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
        let arc = self.owned_of(owner);
        // **The cap is still a check-then-act, and it is still atomic** — but
        // against this instance's lock rather than the whole cell's. §7.2
        // allows one dispatch per instance, so the only contender for it is
        // this instance's own park/wake.
        let mut o = arc.inner.lock();
        let outstanding = *o.per_dispatch.get(&dispatch).unwrap_or(&0);
        if outstanding >= limit {
            return Err(PendingCapError { limit, outstanding });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        o.pending.insert(id);
        *o.per_dispatch.entry(dispatch).or_insert(0) += 1;
        drop(o);
        let _ = self.entries.insert_sync(
            id,
            Entry {
                owner: arc,
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
        let arc = self.owned_of(owner);
        crate::marks::mark(marks, crate::marks::Mark::CancelAllocAcquired);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        arc.inner.lock().pending.insert(id);
        let _ = self.entries.insert_sync(
            id,
            Entry {
                owner: arc,
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
        let acquired = if taking {
            crate::marks::stamp()
        } else {
            crate::marks::NO_STAMP
        };
        // **One hop, no owner lookup.** The entry carries its owner, which is
        // the whole reason this path stopped being the hard one.
        let mut result = Some(result);
        let found = self.entries.update_sync(&id, |_, e| {
            if e.state != State::Pending {
                return Err(e.dispatch);
            }
            e.state = State::Ready;
            e.result = result.take();
            Ok((e.owner.clone(), e.dispatch))
        });
        let (arc, dispatch) = match found {
            None => {
                return Completed {
                    target: WakeTarget::Orphan,
                    dispatch: 0,
                    acquired,
                }
            }
            Some(Err(dispatch)) => {
                return Completed {
                    target: WakeTarget::Duplicate,
                    dispatch,
                    acquired,
                }
            }
            Some(Ok(pair)) => pair,
        };
        // The bucket is released before this lock is taken — see `owned_of`.
        let mut o = arc.inner.lock();
        o.pending.remove(&id);
        if let Some(n) = o.per_dispatch.get_mut(&dispatch) {
            *n = n.saturating_sub(1);
        }
        o.ready.push_back(id);
        drop(o);
        Completed {
            target: WakeTarget::Instance(arc.instance()),
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
        let arc = self.existing(owner)?;
        let mut o = arc.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::TakeReadyAcquired);
        loop {
            let id = o.ready.pop_front()?;
            // Owned held across an `entries` op: the one nesting direction
            // this file allows. `complete` releases its bucket before locking
            // owned, so the pair cannot cycle.
            let Some(result) = self.entries.update_sync(&id, |_, e| {
                e.state = State::Delivered;
                e.result.take()
            }) else {
                continue;
            };
            let Some(result) = result else { continue };
            self.entries.remove_sync(&id);
            return Some((id, result));
        }
    }

    pub fn has_ready(&self, owner: InstanceId) -> bool {
        self.existing(owner)
            .is_some_and(|a| !a.inner.lock().ready.is_empty())
    }




    pub fn pending_for_dispatch(&self, owner: InstanceId, dispatch: u64) -> u32 {
        self.existing(owner)
            .and_then(|a| a.inner.lock().per_dispatch.get(&dispatch).copied())
            .unwrap_or(0)
    }

    pub fn pending_total(&self, owner: InstanceId) -> usize {
        self.existing(owner)
            .map_or(0, |a| a.inner.lock().pending.len())
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
        let Some(arc) = self.existing(owner) else {
            return;
        };
        let mut o = arc.inner.lock();
        crate::marks::mark(marks, crate::marks::Mark::EndDispatchAcquired);
        o.per_dispatch.remove(&dispatch);
        let undelivered: Vec<u64> = o.pending.iter().chain(o.ready.iter()).copied().collect();
        let doomed: Vec<u64> = undelivered
            .into_iter()
            .filter(|id| {
                self.entries
                    .read_sync(id, |_, e| e.dispatch == dispatch)
                    .unwrap_or(false)
            })
            .collect();
        for id in doomed {
            self.entries.remove_sync(&id);
            o.pending.remove(&id);
            o.ready.retain(|r| *r != id);
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
        crate::marks::mark(marks, crate::marks::Mark::ReleaseInstanceAcquired);
        let Some(arc) = self.by_instance.remove_sync(&owner).map(|(_, a)| a) else {
            return;
        };
        self.entries
            .retain_sync(|_, e| !Arc::ptr_eq(&e.owner, &arc));
    }





    /// Every live future id, ascending. For tests and the `wisp dev`
    /// inspector.
    pub fn live_future_ids(&self) -> Vec<u64> {
        let mut v = Vec::new();
        self.entries.retain_sync(|id, _| {
            v.push(*id);
            true
        });
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
        let mut v = Vec::new();
        self.entries.retain_sync(|id, e| {
            if e.import != "ctx_cancel" {
                v.push(*id);
            }
            true
        });
        v.sort_unstable();
        v
    }


    pub fn live_futures(&self) -> usize {
        self.entries.len()
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
