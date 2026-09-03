//! Host-owned futures, completion routing and per-instance resource limits.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use wisp_abi::result::{ResultRecord};

use crate::InstanceId;

/// Never a valid future id: returned when allocation is refused, so a guest
/// that ignores the failure gets no wake rather than someone else's.
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
            "simultaneous pending futures cap exceeded: {} outstanding, limit {}",
            self.outstanding, self.limit
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeTarget {
    /// The owning instance has a deliverable result.
    Instance(InstanceId),
    /// The future had no owner any more; the result is dropped.
    Orphan,
    /// Already completed; not a guest-visible event.
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
    /// Completed and not yet handed to the guest, in completion order.
    ready: VecDeque<u64>,
    /// Pending count per dispatch, the unit the cap applies to.
    per_dispatch: HashMap<u64, u32>,
}

/// One instance's future state, behind its own lock: the per-dispatch cap and
/// the buffered-byte budget are check-then-act inside it, and one dispatch
/// runs per instance at a time, so neither needs a reservation protocol.
#[derive(Debug)]
pub struct OwnedFutures {
    /// Instance identifier returned by completion routing.
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
    /// Keyed by future id, single-key throughout. Every dispatch allocates a
    /// cancellation future, so this is on the hot path even for a guest that
    /// never awaits.
    entries: scc::HashMap<u64, Entry>,
    /// Owner lookup for callers handed an `InstanceId` by the ABI. `complete`
    /// follows `Entry::owner` and never comes here.
    by_instance: scc::HashMap<InstanceId, Arc<OwnedFutures>>,
}

impl Default for FutureTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FutureTable {
    /// Lock order in this file: an `OwnedFutures` lock is never acquired
    /// while an `scc` bucket is held. `take_ready` and `end_dispatch` nest
    /// owned-then-entries; `complete` releases its bucket before locking
    /// owned.
    fn owned_of(&self, owner: InstanceId) -> Arc<OwnedFutures> {
        self.by_instance
            .entry_sync(owner)
            .or_insert_with(|| OwnedFutures::new(owner))
            .get()
            .clone()
    }

    fn existing(&self, owner: InstanceId) -> Option<Arc<OwnedFutures>> {
        self.by_instance.read_sync(&owner, |_, a| a.clone())
    }

    /// Create an empty host future table.
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            entries: scc::HashMap::new(),
            by_instance: scc::HashMap::new(),
        }
    }

    /// `limit` is the application's `pending_futures_per_dispatch`; a refusal is
    /// observed by the guest as `TrapCode::TooManyFutures`.
    pub fn alloc(
        &self,
        owner: InstanceId,
        dispatch: u64,
        import: &'static str,
        limit: u32,
    ) -> Result<u64, PendingCapError> {
        let arc = self.owned_of(owner);
        let mut o = arc.inner.lock();
        let outstanding = *o.per_dispatch.get(&dispatch).unwrap_or(&0);
        if outstanding >= limit {
            return Err(PendingCapError { limit, outstanding });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        o.pending.insert(id);
        *o.per_dispatch.entry(dispatch).or_insert(0) += 1;
        drop(o);
        self.insert_new(
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

    /// Ids come from a counter, so an existing entry under a fresh id is a
    /// table bug worth a loud record rather than a silent overwrite.
    fn insert_new(&self, id: u64, entry: Entry) {
        if self.entries.insert_sync(id, entry).is_err() {
            tracing::error!(future = id, "a fresh future id was already in the table");
        }
    }

    /// The dispatch's implicit cancellation future. Not charged to the
    /// per-dispatch cap, which limits the guest's fan-out and not the
    /// platform's bookkeeping, so it cannot fail.
    pub fn alloc_cancellation(&self, owner: InstanceId, dispatch: u64) -> u64 {
        let arc = self.owned_of(owner);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        arc.inner.lock().pending.insert(id);
        self.insert_new(
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

    /// Makes the result deliverable; the caller enqueues the wake.
    pub fn complete(&self, id: u64, result: ResultRecord) -> WakeTarget {
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
            None => return WakeTarget::Orphan,
            Some(Err(_)) => return WakeTarget::Duplicate,
            Some(Ok(pair)) => pair,
        };
        let mut o = arc.inner.lock();
        o.pending.remove(&id);
        if let Some(n) = o.per_dispatch.get_mut(&dispatch) {
            *n = n.saturating_sub(1);
        }
        o.ready.push_back(id);
        drop(o);
        WakeTarget::Instance(arc.instance())
    }

    /// The next deliverable result for `owner`; called while the instance's
    /// [`crate::CallGuard`] is held, so a completion that landed mid-dispatch
    /// is delivered after the call returns rather than reentrantly.
    pub fn take_ready(&self, owner: InstanceId) -> Option<(u64, ResultRecord)> {
        let arc = self.existing(owner)?;
        let mut o = arc.inner.lock();
        loop {
            let id = o.ready.pop_front()?;
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



    pub fn pending_total(&self, owner: InstanceId) -> usize {
        self.existing(owner)
            .map_or(0, |a| a.inner.lock().pending.len())
    }

    /// Reclaim pending futures owned by the completed dispatch.
    pub fn end_dispatch(&self, owner: InstanceId, dispatch: u64) {
        let Some(arc) = self.existing(owner) else {
            return;
        };
        let mut o = arc.inner.lock();
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

    /// Instance death: every future the instance held is reclaimed. A whole
    /// table scan by owner pointer, off the dispatch path.
    pub fn release_instance(&self, owner: InstanceId) {
        let Some(arc) = self.by_instance.remove_sync(&owner).map(|(_, a)| a) else {
            return;
        };
        self.entries
            .retain_sync(|_, e| !Arc::ptr_eq(&e.owner, &arc));
    }



    /// Every live future id, ascending.
    pub fn live_future_ids(&self) -> Vec<u64> {
        let mut v = Vec::new();
        self.entries.retain_sync(|id, _| {
            v.push(*id);
            true
        });
        v.sort_unstable();
        v
    }

    /// Live futures the guest issued, excluding each dispatch's implicit
    /// cancellation future.
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
        let a = t.alloc(i, 1, "kv_get", 64).expect("alloc");
        let b = t.alloc(i, 1, "kv_get", 64).expect("alloc");
        assert_ne!(a, INVALID_FUTURE, "0 is reserved for a refused allocation");
        assert_ne!(a, b, "ids are never reused");
    }

    #[test]
    fn pending_cap_is_enforced_per_dispatch() {
        let t = table();
        let i = InstanceId(1);
        let limit = 6;

        for n in 0..limit {
            assert!(
                t.alloc(i, 7, "kv_get", limit).is_ok(),
                "alloc {n} refused early"
            );
        }
        let err = t
            .alloc(i, 7, "kv_get", limit)
            .expect_err("the seventh future is over the cap");
        assert_eq!(
            err,
            PendingCapError {
                limit: 6,
                outstanding: 6
            },
            "the refusal names the cap and the count"
        );
        assert!(
            t.alloc(i, 8, "kv_get", limit).is_ok(),
            "another dispatch on the same instance has its own cap"
        );
    }

    #[test]
    fn completing_a_future_frees_cap_headroom() {
        let t = table();
        let i = InstanceId(1);
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(t.alloc(i, 1, "kv_get", 6).expect("alloc"));
        }
        assert!(t.alloc(i, 1, "kv_get", 6).is_err(), "the cap is reached");
        t.complete(ids[0], ResultRecord::Ok(vec![1]));
        assert!(
            t.alloc(i, 1, "kv_get", 6).is_ok(),
            "a completion frees a slot under the cap"
        );
    }

    #[test]
    fn completion_routes_to_the_owner_and_is_idempotent() {
        let t = table();
        let a = InstanceId(1);
        let b = InstanceId(2);
        let fa = t.alloc(a, 1, "kv_get", 64).expect("alloc");
        let fb = t.alloc(b, 1, "kv_get", 64).expect("alloc");
        assert_eq!(
            t.complete(fa, ResultRecord::Ok(vec![1])),
            WakeTarget::Instance(a),
            "a completion names its owner"
        );
        assert_eq!(
            t.complete(fb, ResultRecord::Ok(vec![2])),
            WakeTarget::Instance(b),
            "and the right owner"
        );
        assert_eq!(
            t.complete(fa, ResultRecord::Ok(vec![9])),
            WakeTarget::Duplicate,
            "a second completion is a duplicate"
        );
        assert_eq!(
            t.complete(9_999, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan,
            "an unknown future is an orphan"
        );

        let (id, res) = t.take_ready(a).expect("a ready result");
        assert_eq!(id, fa, "the ready result is the completed future");
        assert!(
            matches!(res, ResultRecord::Ok(v) if v == vec![1]),
            "with the first completion's value, not the duplicate's"
        );
        assert!(t.take_ready(a).is_none(), "nothing else is ready");
    }




    #[test]
    fn ending_a_dispatch_reclaims_its_handles_and_preserves_other_dispatches() {
        let t = table();
        let i = InstanceId(1);
        let doomed = t.alloc(i, 1, "kv_get", 64).expect("alloc");
        let kept = t.alloc(i, 2, "queue_send", 64).expect("alloc");
        t.end_dispatch(i, 1);
        assert_eq!(t.pending_total(i), 1, "a result remains live only for its owning dispatch");
        assert_eq!(
            t.complete(doomed, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan,
            "the reclaimed future's completion has no owner"
        );
        assert_eq!(
            t.complete(kept, ResultRecord::Ok(vec![])),
            WakeTarget::Instance(i),
            "the spared future's completion still wakes the instance"
        );
        t.take_ready(i);
    }

    #[test]
    fn a_completed_but_undelivered_result_does_not_outlive_its_dispatch() {
        let t = table();
        let i = InstanceId(1);
        let doomed = t.alloc(i, 1, "kv_get", 64).expect("alloc");
        let kept = t.alloc(i, 2, "queue_send", 64).expect("alloc");

        assert_eq!(
            t.complete(doomed, ResultRecord::Ok(vec![1])),
            WakeTarget::Instance(i),
            "the plain future completes mid-dispatch"
        );
        assert_eq!(
            t.complete(kept, ResultRecord::Ok(vec![2])),
            WakeTarget::Instance(i),
            "a result remains live only for its owning dispatch"
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
        let f = t.alloc(i, 1, "kv_get", 64).expect("alloc");
        t.release_instance(i);
        assert_eq!(t.live_futures(), 0, "the dead instance's futures are gone");
        assert_eq!(
            t.complete(f, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan,
            "a late completion has no owner"
        );
    }
}
