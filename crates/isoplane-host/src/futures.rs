//! Host-owned futures, completion routing and per-instance resource limits.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use isoplane_abi::result::{FutureError, ResultRecord};

use crate::InstanceId;

/// Never a valid future id: returned when allocation is refused, so a guest
/// that ignores the failure gets no wake rather than someone else's.
pub const INVALID_FUTURE: u64 = 0;

/// The execution identity for instance work, independent of every request.
pub const INSTANCE_WORK: u64 = u64::MAX;

/// Identity of host reactor work, converted from the raw guest ABI at allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FutureId(u64);

impl From<u64> for FutureId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

/// The owner whose retained or pending future limit refused allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingCapScope {
    /// Outstanding work attributed to one dispatch.
    Dispatch,
    /// Retained entries shared by all work in an instance.
    Instance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// An allocation refused by a dispatch or retained-instance entry limit.
pub struct PendingCapError {
    /// Which ownership limit refused the operation.
    pub scope: PendingCapScope,
    /// Maximum entries allowed in that scope.
    pub limit: u32,
    /// Entries already occupying that scope.
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
}


/// Commits a consumed resource only when its result reaches the guest.
/// Dropping before delivery restores the resource's readable state.
pub(crate) trait ResultDelivery: Send {
    fn delivered(&mut self);
}

struct Entry {
    /// Owner for completion routing.
    owner: Arc<OwnedFutures>,
    dispatch: u64,
    import: &'static str,
    /// A wake whose condition the host can recreate in a later dispatch, so
    /// cancelling it with its dispatch abandons no guest work.
    rearmable: bool,
    state: State,
    result: Option<ResultRecord>,
    delivery: Option<Mutex<Box<dyn ResultDelivery>>>,
    task: Option<tokio::task::AbortHandle>,
    waiting: Option<PendingWait>,
    /// When the result became deliverable, so its slice can say how long it
    /// waited for the instance's thread.
    ready_at: Option<std::time::Instant>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.abort_task();
    }
}

#[derive(Debug)]
struct PendingWait {
    span: tracing::Span,
    outcome: &'static str,
}

impl Drop for PendingWait {
    fn drop(&mut self) {
        self.span.record("outcome", self.outcome);
    }
}

impl Entry {
    /// Work the guest is waiting on. A dispatch's cancellation belongs to the
    /// host, and a re-armable wake's condition outlives it.
    fn is_guest_work(&self) -> bool {
        !self.is_control() && !self.rearmable
    }

    fn is_control(&self) -> bool {
        matches!(self.import, "ctx_cancel" | "dispatch_ready")
    }

    fn finish_wait(&mut self, outcome: &'static str) {
        if let Some(mut waiting) = self.waiting.take() {
            waiting.outcome = outcome;
        }
    }

    fn cancel_task(&mut self) {
        self.finish_wait("cancelled");
        self.abort_task();
    }

    fn abort_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Debug)]
struct Owned {
    closed: bool,
    entry_limit: u32,
    retained_entries: u32,
    buffered_bytes: usize,
    /// Registered and not yet completed.
    pending: BTreeSet<u64>,
    /// Completed and not yet handed to the guest, in completion order.
    ready: VecDeque<u64>,
    /// Pending count per dispatch, the unit the cap applies to.
    per_dispatch: HashMap<u64, u32>,
}

impl Default for Owned {
    fn default() -> Self {
        Self {
            closed: false,
            entry_limit: retained_entry_limit(
                crate::bindings::GuestResourceLimits::default().native_handles,
                6,
            ),
            retained_entries: 0,
            buffered_bytes: 0,
            pending: BTreeSet::new(),
            ready: VecDeque::new(),
            per_dispatch: HashMap::new(),
        }
    }
}

pub(crate) fn retained_entry_limit(native_handles: usize, pending_per_request: u32) -> u32 {
    native_handles
        .saturating_add(crate::DISPATCHES_PER_INSTANCE.saturating_mul(pending_per_request as usize))
        .min(u32::MAX as usize) as u32
}

fn result_bytes(result: &ResultRecord) -> usize {
    match result {
        ResultRecord::Ok(bytes) => bytes.capacity(),
        ResultRecord::Err(FutureError::Denied(message) | FutureError::Io(message)) => {
            message.capacity()
        }
        ResultRecord::Err(
            FutureError::Cancelled
            | FutureError::NotFound
            | FutureError::BufferLimit,
        ) => 0,
    }
}

impl Owned {
    fn removed(&mut self, entry: &Entry) {
        if !entry.is_control() {
            self.retained_entries -= 1;
        }
        self.buffered_bytes -= entry.result.as_ref().map_or(0, result_bytes);
    }
}

/// One instance's retained results and in-flight read reservations.
#[derive(Debug)]
pub struct OwnedFutures {
    /// Instance identifier returned by completion routing.
    instance: AtomicU64,
    inner: Mutex<Owned>,
    space_available: tokio::sync::Notify,
}

impl OwnedFutures {
    pub fn new(instance: InstanceId) -> Arc<OwnedFutures> {
        Arc::new(OwnedFutures {
            instance: AtomicU64::new(instance.0),
            inner: Mutex::new(Owned::default()),
            space_available: tokio::sync::Notify::new(),
        })
    }

    pub fn instance(&self) -> InstanceId {
        InstanceId(self.instance.load(Ordering::Acquire))
    }

}

/// Space held for a read's undelivered result.
pub(crate) struct ReadSpace {
    future: FutureId,
    owner: Arc<OwnedFutures>,
    bytes: usize,
}

impl ReadSpace {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for ReadSpace {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.owner.inner.lock().buffered_bytes -= self.bytes;
            self.owner.space_available.notify_waiters();
        }
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
    /// Per-instance byte budget for undelivered results and reserved read space.
    buffered_budget_bytes: usize,
}

impl FutureTable {
    /// Release one resource's outstanding future without disturbing other work.
    /// A completion already delivered to the guest has nothing left to cancel.
    pub fn cancel_owned(&self, owner: InstanceId, id: FutureId) -> bool {
        let Some(arc) = self.existing(owner) else {
            return false;
        };
        let mut owned = arc.inner.lock();
        let Some((_, mut entry)) = self
            .entries
            .remove_if_sync(&id.0, |entry| Arc::ptr_eq(&entry.owner, &arc))
        else {
            return false;
        };
        if entry.state == State::Pending && !entry.is_control() {
            if let Some(count) = owned.per_dispatch.get_mut(&entry.dispatch) {
                *count = count.saturating_sub(1);
            }
        }
        owned.pending.remove(&id.0);
        owned.ready.retain(|candidate| *candidate != id.0);
        owned.removed(&entry);
        arc.space_available.notify_waiters();
        entry.cancel_task();
        true
    }

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
    pub fn new(buffered_budget_bytes: usize) -> Self {
        Self {
            next: AtomicU64::new(1),
            entries: scc::HashMap::new(),
            by_instance: scc::HashMap::new(),
            buffered_budget_bytes,
        }
    }

    pub(crate) fn set_entry_limit(&self, owner: InstanceId, limit: u32) {
        self.owned_of(owner).inner.lock().entry_limit = limit;
    }

    /// Allocates within the dispatch's pending limit and the instance's retained
    /// entry limit. The error identifies which scope refused the operation.
    pub fn alloc(
        &self,
        owner: InstanceId,
        dispatch: u64,
        import: &'static str,
        limit: u32,
    ) -> Result<u64, PendingCapError> {
        self.alloc_entry(owner, dispatch, import, class, limit, false)
    }

    /// Like [`FutureTable::alloc`], for a wake the host recreates when the
    /// guest next waits on the same condition. Ending its dispatch cancels it
    /// without counting as abandoned guest work.
    pub fn alloc_rearmable(
        &self,
        owner: InstanceId,
        dispatch: u64,
        import: &'static str,
        limit: u32,
    ) -> Result<u64, PendingCapError> {
        self.alloc_entry(owner, dispatch, import, class, limit, true)
    }

    fn alloc_entry(
        &self,
        owner: InstanceId,
        dispatch: u64,
        import: &'static str,
        limit: u32,
        rearmable: bool,
    ) -> Result<u64, PendingCapError> {
        let arc = self.owned_of(owner);
        let mut o = arc.inner.lock();
        if o.retained_entries >= o.entry_limit {
            return Err(PendingCapError {
                scope: PendingCapScope::Instance,
                limit: o.entry_limit,
                outstanding: o.retained_entries,
            });
        }
        let outstanding = *o.per_dispatch.get(&dispatch).unwrap_or(&0);
        if outstanding >= limit {
            return Err(PendingCapError {
                scope: PendingCapScope::Dispatch,
                limit,
                outstanding,
            });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        o.pending.insert(id);
        o.retained_entries += 1;
        *o.per_dispatch.entry(dispatch).or_insert(0) += 1;
        drop(o);
        self.insert_new(
            id,
            Entry {
                owner: arc,
                dispatch,
                import,
                rearmable,
                state: State::Pending,
                result: None,
                delivery: None,
                task: None,
                waiting: Some(PendingWait {
                    span: tracing::info_span!(
                        "host_future_wait",
                        instance = %owner,
                        dispatch_id = dispatch,
                        future_id = id,
                        import,
                        outcome = tracing::field::Empty,
                    ),
                    outcome: "discarded",
                }),
                ready_at: None,
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
        self.alloc_control(owner, dispatch, "ctx_cancel")
    }

    /// Coalesces cooperative continuation requests independently of the guest operation cap.
    pub(crate) fn alloc_slice_request(&self, owner: InstanceId, dispatch: u64) -> u64 {
        self.alloc_control(owner, dispatch, "dispatch_ready")
    }

    fn alloc_control(&self, owner: InstanceId, dispatch: u64, import: &'static str) -> u64 {
        let arc = self.owned_of(owner);
        let mut owned = arc.inner.lock();
        if let Some(id) = owned.pending.iter().chain(owned.ready.iter()).find(|id| {
            self.entries
                .read_sync(*id, |_, entry| {
                    entry.dispatch == dispatch && entry.import == import
                })
                .unwrap_or(false)
        }) {
            return *id;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        owned.pending.insert(id);
        drop(owned);
        self.insert_new(
            id,
            Entry {
                owner: arc,
                dispatch,
                import,
                rearmable: false,
                state: State::Pending,
                result: None,
                delivery: None,
                task: None,
                waiting: None,
                ready_at: None,
            },
        );
        id
    }

    /// Makes the result deliverable; the caller enqueues the wake.
    pub fn complete(&self, id: u64, result: ResultRecord) -> WakeTarget {
        self.complete_with_delivery(id, result, None)
    }

    pub(crate) async fn reserve_read(
        &self,
        id: FutureId,
        max_bytes: usize,
    ) -> Result<ReadSpace, FutureError> {
        let id = id.0;
        let arc = self
            .entries
            .read_sync(&id, |_, entry| entry.owner.clone())
            .ok_or(FutureError::Cancelled)?;
        let bytes = max_bytes.min(self.buffered_budget_bytes);
        if bytes == 0 {
            return Err(FutureError::BufferLimit);
        }
        loop {
            let available = arc.space_available.notified();
            tokio::pin!(available);
            available.as_mut().enable();
            {
                let mut owned = arc.inner.lock();
                if owned.closed || !owned.pending.contains(&id) {
                    return Err(FutureError::Cancelled);
                }
                if bytes
                    <= self
                        .buffered_budget_bytes
                        .saturating_sub(owned.buffered_bytes)
                {
                    owned.buffered_bytes += bytes;
                    return Ok(ReadSpace {
                        future: FutureId(id),
                        owner: arc.clone(),
                        bytes,
                    });
                }
            }
            available.await;
        }
    }

    pub(crate) fn complete_read(
        &self,
        result: ResultRecord,
        space: ReadSpace,
        delivery: Option<Box<dyn ResultDelivery>>,
    ) -> WakeTarget {
        self.complete_reserved(space.future.0, result, Some(space), delivery)
    }

    pub(crate) fn complete_with_delivery(
        &self,
        id: u64,
        result: ResultRecord,
        delivery: Option<Box<dyn ResultDelivery>>,
    ) -> WakeTarget {
        self.complete_reserved(id, result, None, delivery)
    }

    fn complete_reserved(
        &self,
        id: u64,
        result: ResultRecord,
        mut space: Option<ReadSpace>,
        mut delivery: Option<Box<dyn ResultDelivery>>,
    ) -> WakeTarget {
        let Some(arc) = self.entries.read_sync(&id, |_, entry| entry.owner.clone()) else {
            return WakeTarget::Orphan;
        };
        let mut owned = arc.inner.lock();
        if let Some(space) = space.as_mut() {
            owned.buffered_bytes -= std::mem::take(&mut space.bytes);
            arc.space_available.notify_waiters();
        }
        let found = self.entries.update_sync(&id, |_, entry| {
            if entry.state != State::Pending {
                return None;
            }
            let bytes = result_bytes(&result);
            let result = if bytes > self.buffered_budget_bytes.saturating_sub(owned.buffered_bytes) {
                tracing::warn!(instance = %arc.instance(), future = id, limit_bytes = self.buffered_budget_bytes,
                    result_bytes = bytes, "instance buffered-result byte limit exceeded");
                ResultRecord::Err(FutureError::BufferLimit)
            } else {
                owned.buffered_bytes += bytes;
                entry.delivery = delivery.take().map(Mutex::new);
                result
            };
            entry.state = State::Ready;
            entry.result = Some(result);
            entry.ready_at = Some(std::time::Instant::now());
            entry.finish_wait("ready");
            Some((entry.dispatch, entry.is_control()))
        });
        let (dispatch, control) = match found {
            None => return WakeTarget::Orphan,
            Some(None) => return WakeTarget::Duplicate,
            Some(Some(completed)) => completed,
        };
        owned.pending.remove(&id);
        if !control {
            if let Some(count) = owned.per_dispatch.get_mut(&dispatch) {
                *count -= 1;
            }
        }
        owned.ready.push_back(id);
        WakeTarget::Instance(arc.instance())
    }

    /// Attaches reactor work to its future. Work attached after its owner ended
    /// is cancelled immediately, including a spawn racing dispatch teardown.
    pub fn attach_task(&self, id: u64, task: tokio::task::AbortHandle) {
        let mut task = Some(task);
        self.entries.update_sync(&id, |_, entry| {
            if entry.state == State::Pending && entry.task.is_none() {
                entry.task = task.take();
            }
        });
        if let Some(task) = task {
            task.abort();
        }
    }

    /// Takes a request result without executing instance work under that request.
    pub fn take_ready(&self, owner: InstanceId) -> Option<Ready> {
        self.take_ready_scope(owner, false)
    }

    /// Takes one instance result for a background slice.
    pub fn take_background_ready(&self, owner: InstanceId) -> Option<Ready> {
        self.take_ready_scope(owner, true)
    }

    fn take_ready_scope(&self, owner: InstanceId, background: bool) -> Option<Ready> {
        let arc = self.existing(owner)?;
        let mut o = arc.inner.lock();
        let mut index = 0;
        while let Some(&id) = o.ready.get(index) {
            let Some(dispatch) = self.entries.read_sync(&id, |_, e| e.dispatch) else {
                o.ready.remove(index);
                continue;
            };
            if (dispatch == INSTANCE_WORK) != background {
                index += 1;
                continue;
            }
            o.ready.remove(index);
            let Some((_, mut entry)) = self.entries.remove_sync(&id) else {
                continue;
            };
            o.removed(&entry);
            arc.space_available.notify_waiters();
            let Some(result) = entry.result.take() else {
                continue;
            };
            if let Some(delivery) = &mut entry.delivery {
                delivery.get_mut().delivered();
            }
            let ready_at = entry.ready_at;
            return Some(Ready {
                future: id,
                dispatch,
                result,
                waited: if background {
                    None
                } else {
                    ready_at.map(|at| at.elapsed())
                },
            });
        }
        None
    }

    pub fn has_ready(&self, owner: InstanceId) -> bool {
        self.existing(owner)
            .is_some_and(|a| !a.inner.lock().ready.is_empty())
    }

    /// Whether `owner` has a request result ready to run.
    pub fn has_runnable(&self, owner: InstanceId) -> bool {
        let Some(arc) = self.existing(owner) else {
            return false;
        };
        let o = arc.inner.lock();
        o.ready.iter().any(|id| {
            self.entries
                .read_sync(id, |_, entry| entry.dispatch != INSTANCE_WORK)
                .unwrap_or(false)
        })
    }

    /// Whether an instance result is ready, without consuming it.
    pub(crate) fn has_background_ready(&self, owner: InstanceId) -> bool {
        self.existing(owner).is_some_and(|arc| {
            arc.inner.lock().ready.iter().any(|id| {
                self.entries
                    .read_sync(id, |_, entry| entry.dispatch == INSTANCE_WORK)
                    .unwrap_or(false)
            })
        })
    }

    /// Makes `dispatch`'s futures the instance's, pending or undelivered, and
    /// returns how many moved. Moving them does not touch their reactor work.
    pub fn hand_to_instance(&self, owner: InstanceId, dispatch: u64) -> usize {
        let Some(arc) = self.existing(owner) else {
            return 0;
        };
        let mut o = arc.inner.lock();
        let ids: Vec<u64> = o.pending.iter().chain(o.ready.iter()).copied().collect();
        let mut moved = 0;
        for id in ids {
            let pending = o.pending.contains(&id);
            let handed = self
                .entries
                .update_sync(&id, |_, e| {
                    let wanted = e.dispatch == dispatch && !e.is_control();
                    if wanted {
                        e.dispatch = INSTANCE_WORK;
                    }
                    wanted
                })
                .unwrap_or(false);
            if !handed {
                continue;
            }
            moved += 1;
            if pending {
                if let Some(count) = o.per_dispatch.get_mut(&dispatch) {
                    *count = count.saturating_sub(1);
                }
                *o.per_dispatch.entry(INSTANCE_WORK).or_insert(0) += 1;
            }
        }
        moved
    }

    /// The owner's pending futures that `import` created.
    pub fn pending_for_import(&self, owner: InstanceId, import: &str) -> usize {
        let Some(arc) = self.existing(owner) else {
            return 0;
        };
        let o = arc.inner.lock();
        o.pending
            .iter()
            .filter(|id| {
                self.entries
                    .read_sync(*id, |_, entry| entry.import == import)
                    .unwrap_or(false)
            })
            .count()
    }

    /// Whether the owner has a pending or undelivered future the guest is
    /// waiting on, other than the instance's work, which no dispatch waits for.
    pub fn holds_guest_work(&self, owner: InstanceId) -> bool {
        let Some(arc) = self.existing(owner) else {
            return false;
        };
        let o = arc.inner.lock();
        o.pending.iter().chain(o.ready.iter()).any(|id| {
            self.entries
                .read_sync(id, |_, entry| {
                    entry.is_guest_work() && entry.dispatch != INSTANCE_WORK
                })
                .unwrap_or(false)
        })
    }

    /// Whether `dispatch` has a pending or undelivered future the guest is
    /// waiting on, so the dispatch is not finished even if its response is.
    pub fn dispatch_holds_guest_work(&self, owner: InstanceId, dispatch: u64) -> bool {
        let Some(arc) = self.existing(owner) else {
            return false;
        };
        let o = arc.inner.lock();
        o.pending.iter().chain(o.ready.iter()).any(|id| {
            self.entries
                .read_sync(id, |_, entry| {
                    entry.dispatch == dispatch && entry.is_guest_work()
                })
                .unwrap_or(false)
        })
    }

    /// `dispatch`'s pending futures that `import` created.
    pub fn pending_of_dispatch(&self, owner: InstanceId, dispatch: u64, import: &str) -> Vec<u64> {
        let Some(arc) = self.existing(owner) else {
            return Vec::new();
        };
        let o = arc.inner.lock();
        o.pending
            .iter()
            .copied()
            .filter(|id| {
                self.entries
                    .read_sync(id, |_, entry| {
                        entry.dispatch == dispatch && entry.import == import
                    })
                    .unwrap_or(false)
            })
            .collect()
    }

    pub fn pending_total(&self, owner: InstanceId) -> usize {
        self.existing(owner)
            .map_or(0, |a| a.inner.lock().pending.len())
    }

    /// Whether this owner's operation still needs a completion, independent of task cleanup.
    pub(crate) fn is_pending(&self, owner: InstanceId, id: FutureId) -> bool {
        self.existing(owner)
            .is_some_and(|owned| owned.inner.lock().pending.contains(&id.0))
    }

    /// Releases all dispatch-owned futures, whether
    /// still pending or completed and undelivered; a result left in `ready`
    /// would be served to the next dispatch on this instance as a wake for a
    /// future it never issued.
    /// Returns whether guest work was abandoned, excluding implicit
    /// cancellation bookkeeping and re-armable wakes.
    pub fn end_dispatch(&self, owner: InstanceId, dispatch: u64) -> bool {
        let Some(arc) = self.existing(owner) else {
            return false;
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
        let mut abandoned_guest_work = false;
        for id in doomed {
            if let Some((_, mut entry)) = self.entries.remove_sync(&id) {
                abandoned_guest_work |= entry.is_guest_work();
                o.removed(&entry);
                entry.cancel_task();
            }
            o.pending.remove(&id);
            o.ready.retain(|r| *r != id);
        }
        arc.space_available.notify_waiters();
        abandoned_guest_work
    }

    /// An instance going idle: its futures are released, unless some are the
    /// instance's work, which stays pending, with its reactor work, for a later
    /// dispatch to run. Discarding the instance releases those.
    pub fn release_idle(&self, owner: InstanceId) {
        let holds_instance_work = self.existing(owner).is_some_and(|arc| {
            let o = arc.inner.lock();
            o.pending.iter().chain(o.ready.iter()).any(|id| {
                self.entries
                    .read_sync(id, |_, entry| entry.dispatch == INSTANCE_WORK)
                    .unwrap_or(false)
            })
        });
        if !holds_instance_work {
            self.release_instance(owner);
        }
    }

    /// Instance death: every future the instance held is reclaimed. A whole
    /// table scan by owner pointer, off the dispatch path.
    pub fn release_instance(&self, owner: InstanceId) {
        let Some(arc) = self.by_instance.remove_sync(&owner).map(|(_, a)| a) else {
            return;
        };
        arc.inner.lock().closed = true;
        arc.space_available.notify_waiters();
        // Aborting a task can drop what it holds on another thread at once, so
        // abort only after the entries are gone: whoever observes the drop
        // then sees the future released too.
        let mut tasks = Vec::new();
        self.entries.retain_sync(|_, entry| {
            if Arc::ptr_eq(&entry.owner, &arc) {
                entry.finish_wait("cancelled");
                tasks.extend(entry.task.take());
                false
            } else {
                true
            }
        });
        for task in tasks {
            task.abort();
        }
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
            if !e.is_control() {
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

/// A completed future ready for delivery to its dispatch.
#[derive(Debug)]
pub struct Ready {
    pub future: u64,
    pub dispatch: u64,
    pub result: ResultRecord,
    /// Duration since the result became ready, when a timestamp is available.
    pub waited: Option<std::time::Duration>,
}

impl Ready {
    /// The future and its result, for a caller that already knows the dispatch.
    pub fn into_pair(self) -> (u64, ResultRecord) {
        (self.future, self.result)
    }
}


#[cfg(test)]
mod tests {
    use super::*;


    #[tokio::test]
    async fn reads_wait_for_space_and_release_it_on_delivery_or_cancellation() {
        let table = FutureTable::new(8);
        let owner = InstanceId(1);
        let first = table
            .alloc(owner, 1, "req_body_read", 2)
            .expect("first read");
        let second = table
            .alloc(owner, 2, "req_body_read", 2)
            .expect("second read");
        let space = table
            .reserve_read(first.into(), 100)
            .await
            .expect("oversized read is shortened");
        assert_eq!(space.bytes(), 8);
        let waiting = table.reserve_read(second.into(), 8);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        table.complete_read(ResultRecord::Ok(vec![1; 8]), space, None);
        assert!(
            futures::poll!(&mut waiting).is_pending(),
            "undelivered bytes still occupy space"
        );
        assert_eq!(
            table.take_ready(owner).expect("first result").result,
            ResultRecord::Ok(vec![1; 8])
        );
        let space = waiting.await.expect("delivery admits the next read");
        drop(space);
        let space = table
            .reserve_read(second.into(), 8)
            .await
            .expect("abandoning a read releases its space");
        table.complete_read(ResultRecord::Ok(vec![2; 8]), space, None);
        let third = table
            .alloc(owner, 3, "req_body_read", 2)
            .expect("third read");
        let waiting = table.reserve_read(third.into(), 8);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        assert!(table.cancel_owned(owner, second.into()));
        let space = waiting
            .await
            .expect("cancelling a result releases its space");
        let fourth = table
            .alloc(owner, 4, "req_body_read", 2)
            .expect("fourth read");
        let waiting = table.reserve_read(fourth.into(), 8);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        table.release_instance(owner);
        assert!(matches!(waiting.await, Err(FutureError::Cancelled)));
        drop(space);
        let denied = FutureTable::new(0);
        let id = denied
            .alloc(owner, 1, "req_body_read", 1)
            .expect("read entry");
        assert!(matches!(
            denied.reserve_read(id.into(), 8).await,
            Err(FutureError::BufferLimit)
        ));
    }

    fn table() -> FutureTable {
        FutureTable::new(64 * 1024)
    }

    async fn held_task() -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            started
                .send(())
                .expect("the test must observe reactor work starting");
            held.await
                .expect("the test must release surviving reactor work");
        });
        running.await.expect("the reactor work must start");
        (task, release)
    }

    async fn assert_cancelled(task: tokio::task::JoinHandle<()>) {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), task)
            .await
            .expect("cancelled reactor work must not hang")
            .expect_err("released ownership must cancel its reactor work");
        assert!(
            outcome.is_cancelled(),
            "reactor work must be cancelled, not panic"
        );
    }

    #[tokio::test]
    async fn delivering_an_externally_completed_future_releases_its_reactor_work() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "clock_deadline_in", 8)
            .expect("future allocation");
        let (task, _release) = held_task().await;
        table.attach_task(id, task.abort_handle());
        assert!(
            matches!(table.complete(id, ResultRecord::Ok(vec![])), WakeTarget::Instance(instance) if instance == owner)
        );
        assert_eq!(
            table.take_ready(owner).map(Ready::into_pair),
            Some((id, ResultRecord::Ok(vec![])))
        );
        assert_cancelled(task).await;
        assert_eq!(table.live_futures(), 0);
    }

    #[tokio::test]
    async fn a_rearmable_wake_ends_with_its_dispatch_without_abandoning_guest_work() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc_rearmable(owner, 1, "poll_call", 8)
            .expect("future allocation");
        let (task, _release) = held_task().await;
        table.attach_task(id, task.abort_handle());
        assert!(
            !table.end_dispatch(owner, 1),
            "a wake the host can re-arm must not count as abandoned guest work"
        );
        assert_cancelled(task).await;
        assert_eq!(
            table.live_futures(),
            0,
            "dispatch teardown must still release the wake's record"
        );
    }

    #[tokio::test]
    async fn ending_a_dispatch_cancels_its_pending_reactor_work() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "http_fetch", 8)
            .expect("future allocation");
        let (task, _release) = held_task().await;
        table.attach_task(id, task.abort_handle());
        table.end_dispatch(owner, 1);
        assert_cancelled(task).await;
        assert_eq!(
            table.live_futures(),
            0,
            "dispatch teardown must release the future record"
        );
    }

    #[tokio::test]
    async fn work_attached_after_dispatch_teardown_is_cancelled() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "http_fetch", 8)
            .expect("future allocation");
        table.end_dispatch(owner, 1);
        let (task, _release) = held_task().await;
        table.attach_task(id, task.abort_handle());
        assert_cancelled(task).await;
    }

    #[tokio::test]
    async fn instance_death_cancels_its_reactor_work() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "http_fetch", 8)
            .expect("future allocation");
        let (task, _release) = held_task().await;
        table.attach_task(id, task.abort_handle());
        table.release_instance(owner);
        assert_cancelled(task).await;
    }



    #[test]
    fn ids_are_never_zero_and_never_reused() {
        let t = table();
        let i = InstanceId(1);
        let a = t.alloc(i, 1, "clock_deadline_in", 64).expect("alloc");
        let b = t.alloc(i, 1, "clock_deadline_in", 64).expect("alloc");
        assert_ne!(a, INVALID_FUTURE, "0 is reserved for a refused allocation");
        assert_ne!(a, b, "ids are never reused");
    }

    #[test]
    fn an_instance_cannot_cancel_another_instances_reactor_future() {
        let table = table();
        let owner = InstanceId(1);
        let intruder = InstanceId(2);
        let id = table
            .alloc(owner, 1, "poll_call", 1)
            .expect("the owner must obtain its future");
        table
            .alloc(intruder, 1, "poll_call", 1)
            .expect("the other instance must have its own ownership record");
        assert!(
            !table.cancel_owned(intruder, FutureId::from(id)),
            "a different instance must not cancel the owner's timer"
        );
        assert_eq!(
            table.pending_total(owner),
            1,
            "a refused cancellation must retain the owner's future"
        );
    }

    #[test]
    fn dropping_a_pending_resource_releases_its_future_budget() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "poll_call", 1)
            .expect("the timer must allocate its future");
        assert!(
            table.cancel_owned(owner, FutureId::from(id)),
            "the owner must cancel its pending timer"
        );
        assert_eq!(
            table.pending_total(owner),
            0,
            "a dropped timer must leave no pending work"
        );
        assert!(
            table.alloc(owner, 1, "poll_call", 1).is_ok(),
            "a dropped timer must release its pending-future slot"
        );
        assert_eq!(
            table.complete(id, ResultRecord::Ok(Vec::new())),
            WakeTarget::Orphan,
            "a completion racing disposal must not wake the guest"
        );
    }

    #[test]
    fn dropping_a_ready_resource_removes_its_queued_notification() {
        let table = table();
        let owner = InstanceId(1);
        let id = table
            .alloc(owner, 1, "poll_call", 1)
            .expect("the timer must allocate its future");
        table.complete(id, ResultRecord::Ok(Vec::new()));
        assert!(
            table.cancel_owned(owner, FutureId::from(id)),
            "the owner must release its completed resource"
        );
        assert!(
            table.take_ready(owner).is_none(),
            "a dropped resource must not leave a stale wake notification"
        );
    }

    #[test]
    fn pending_cap_is_enforced_per_dispatch() {
        let t = table();
        let i = InstanceId(1);
        let limit = 6;

        for n in 0..limit {
            assert!(
                t.alloc(i, 7, "clock_deadline_in", limit).is_ok(),
                "alloc {n} refused early"
            );
        }
        let err = t
            .alloc(i, 7, "clock_deadline_in", limit)
            .expect_err("the seventh future is over the cap");
        assert_eq!(
            err,
            PendingCapError {
                scope: PendingCapScope::Dispatch,
                limit: 6,
                outstanding: 6
            },
            "the refusal names the cap and the count"
        );
        let continuation = t.alloc_slice_request(i, 7);
        assert_eq!(t.alloc_slice_request(i, 7), continuation);
        t.complete(continuation, ResultRecord::Ok(Vec::new()));
        assert_eq!(t.alloc_slice_request(i, 7), continuation);
        let cancellation = t.alloc_cancellation(i, 7);
        t.complete(cancellation, ResultRecord::Err(FutureError::Cancelled));
        assert_eq!(
            t.alloc(i, 7, "clock_deadline_in", limit),
            Err(err),
            "control completions do not free guest operation capacity"
        );
        assert_eq!(t.hand_to_instance(i, 7), limit as usize);
        assert_eq!(t.take_ready(i).expect("continuation").future, continuation);
        assert_eq!(t.take_ready(i).expect("cancellation").future, cancellation);
        assert!(
            t.alloc(i, 8, "clock_deadline_in", limit).is_ok(),
            "another dispatch on the same instance has its own cap"
        );
    }

    #[test]
    fn completing_a_future_frees_cap_headroom() {
        let t = table();
        let i = InstanceId(1);
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(t.alloc(i, 1, "clock_deadline_in", 6).expect("alloc"));
        }
        assert!(
            t.alloc(i, 1, "clock_deadline_in", 6).is_err(),
            "the cap is reached"
        );
        t.complete(ids[0], ResultRecord::Ok(vec![1]));
        assert!(
            t.alloc(i, 1, "clock_deadline_in", 6).is_ok(),
            "a completion frees a slot under the cap"
        );
    }

    #[test]
    fn completion_routes_to_the_owner_and_is_idempotent() {
        let t = table();
        let a = InstanceId(1);
        let b = InstanceId(2);
        let fa = t.alloc(a, 1, "clock_deadline_in", 64).expect("alloc");
        let fb = t.alloc(b, 1, "clock_deadline_in", 64).expect("alloc");
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

        let (id, res) = t
            .take_ready(a)
            .map(Ready::into_pair)
            .expect("a ready result");
        assert_eq!(id, fa, "the ready result is the completed future");
        assert!(
            matches!(res, ResultRecord::Ok(v) if v == vec![1]),
            "with the first completion's value, not the duplicate's"
        );
        assert!(t.take_ready(a).is_none(), "nothing else is ready");
    }

    #[test]
    fn instance_results_are_delivered_only_to_background_slices() {
        let t = table();
        let owner = InstanceId(1);
        let left = t.alloc(owner, 1, "clock_deadline_in", 8).expect("alloc");
        let own = t.alloc(owner, 2, "clock_deadline_in", 8).expect("alloc");
        assert_eq!(t.hand_to_instance(owner, 1), 1);
        assert!(
            !t.dispatch_holds_guest_work(owner, 1),
            "the handed work belongs to no dispatch"
        );
        t.complete(left, ResultRecord::Ok(vec![]));
        assert!(
            !t.has_runnable(owner),
            "instance work does not make request work runnable"
        );
        assert!(
            t.take_ready(owner).is_none(),
            "request slices do not consume instance results"
        );
        t.complete(own, ResultRecord::Ok(vec![]));
        assert!(
            t.holds_guest_work(owner),
            "a dispatch's own undelivered result is still its work"
        );
        let first = t.take_ready(owner).expect("a dispatch's own result");
        assert_eq!(
            (first.future, first.dispatch),
            (own, 2),
            "a dispatch's own result is not held up behind the instance's"
        );
        assert!(
            !t.holds_guest_work(owner),
            "no dispatch waits for the instance's work"
        );
        assert!(t.has_background_ready(owner));
        let hosted = t
            .take_background_ready(owner)
            .expect("the instance's result");
        assert_eq!(
            (hosted.future, hosted.dispatch),
            (left, INSTANCE_WORK),
            "instance work keeps its background identity"
        );
    }

    #[test]
    fn a_handover_moves_only_what_it_names() {
        let t = table();
        let owner = InstanceId(1);
        let work = t.alloc(owner, 1, "clock_deadline_in", 8).expect("alloc");
        t.alloc_rearmable(owner, 1, "poll_call", 8)
            .expect("alloc");
        t.alloc_cancellation(owner, 1);
        assert_eq!(
            t.hand_to_instance(owner, 1),
            2,
            "all work moves; cancellation stays with the request"
        );
        assert!(
            !t.end_dispatch(owner, 1),
            "nothing is abandoned when it ends"
        );
        t.complete(work, ResultRecord::Ok(vec![]));
        assert!(t.take_background_ready(owner).is_some());
    }

    #[test]
    fn an_idle_instance_keeps_only_its_own_work() {
        let t = table();
        let owner = InstanceId(1);
        t.alloc(owner, 1, "clock_deadline_in", 8).expect("alloc");
        t.release_idle(owner);
        assert_eq!(
            t.live_futures(),
            0,
            "a finished dispatch leaves nothing behind"
        );

        let kept = t.alloc(owner, 2, "clock_deadline_in", 8).expect("alloc");
        t.hand_to_instance(owner, 2);
        t.release_idle(owner);
        assert_eq!(
            t.live_future_ids(),
            vec![kept],
            "the instance's work waits for a later dispatch"
        );
        t.release_instance(owner);
        assert_eq!(t.live_futures(), 0, "discarding the instance releases it");
    }



    #[test]
    fn retained_results_and_handover_share_one_instance_limit() {
        let t = FutureTable::new(8);
        let i = InstanceId(1);
        t.set_entry_limit(i, 2);
        let small = t.alloc(i, 1, "clock_deadline_in", 64).expect("alloc");
        let big = t.alloc(i, 1, "clock_deadline_in", 64).expect("alloc");
        t.complete(small, ResultRecord::Ok(vec![0u8; 4]));
        t.complete(big, ResultRecord::Ok(vec![0u8; 4096]));
        assert_eq!(t.hand_to_instance(i, 1), 2);
        assert!(
            t.alloc(i, 2, "clock_deadline_in", 64).is_err(),
            "handover and completion keep their entry reservations"
        );
        assert_eq!(
            t.take_background_ready(i).expect("small result").result,
            ResultRecord::Ok(vec![0; 4])
        );
        assert_eq!(
            t.take_background_ready(i).expect("rejected result").result,
            ResultRecord::Err(FutureError::BufferLimit)
        );
        let next = t
            .alloc(i, 2, "clock_deadline_in", 64)
            .expect("released entry capacity");
        t.complete(next, ResultRecord::Ok(vec![0; 8]));
        assert!(t.cancel_owned(i, next.into()));
        let last = t
            .alloc(i, 3, "clock_deadline_in", 64)
            .expect("cancelled capacity");
        t.complete(last, ResultRecord::Ok(vec![1; 8]));
        assert_eq!(
            t.take_ready(i).expect("capacity after cancellation").result,
            ResultRecord::Ok(vec![1; 8])
        );
    }

    #[test]
    fn implicit_cancellation_and_delivered_work_do_not_prevent_warm_reuse() {
        let t = table();
        let owner = InstanceId(1);
        t.alloc_cancellation(owner, 1);
        let id = t
            .alloc(owner, 1, "clock_deadline_in", 64)
            .expect("guest work must allocate");
        t.complete(id, ResultRecord::Ok(vec![1]));
        assert!(
            t.take_ready(owner).is_some(),
            "the guest must receive its completed work"
        );
        assert!(
            !t.end_dispatch(owner, 1),
            "delivered work and implicit cancellation leave no guest operation abandoned"
        );
        assert_eq!(
            t.live_futures(),
            0,
            "dispatch bookkeeping must still be reclaimed"
        );
        assert!(
            !t.end_dispatch(owner, 1),
            "repeated cleanup must not invent abandoned work"
        );
    }

    #[test]
    fn ending_a_dispatch_reclaims_only_its_own_handles() {
        let t = table();
        let owner = InstanceId(1);
        let doomed = t
            .alloc(owner, 1, "clock_deadline_in", 64)
            .expect("alloc");
        let other = t.alloc(owner, 2, "queue_send", 64).expect("alloc");
        assert!(
            t.end_dispatch(owner, 1),
            "pending guest work must prevent warm reuse"
        );
        assert_eq!(
            t.pending_total(owner),
            1,
            "another active dispatch must retain its future"
        );
        assert_eq!(
            t.complete(doomed, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan,
            "completed dispatch work must not wake its instance"
        );
        assert_eq!(
            t.complete(other, ResultRecord::Ok(vec![])),
            WakeTarget::Instance(owner),
            "active dispatch work must still wake its owner"
        );
    }


    #[test]
    fn a_completed_but_undelivered_result_does_not_outlive_its_dispatch() {
        let t = table();
        let owner = InstanceId(1);
        let id = t
            .alloc(owner, 1, "clock_deadline_in", 64)
            .expect("alloc");
        assert_eq!(
            t.complete(id, ResultRecord::Ok(vec![1])),
            WakeTarget::Instance(owner),
            "the future completes during dispatch"
        );
        assert!(
            t.end_dispatch(owner, 1),
            "undelivered guest results must prevent warm reuse"
        );
        assert_eq!(
            t.take_ready(owner).map(Ready::into_pair),
            None,
            "a ready result must not reach the next dispatch"
        );
        assert!(!t.has_ready(owner), "no result may outlive its dispatch");
    }

    #[test]
    fn releasing_an_instance_reclaims_every_handle() {
        let t = table();
        let i = InstanceId(1);
        let f = t.alloc(i, 1, "clock_deadline_in", 64).expect("alloc");
        t.release_instance(i);
        assert_eq!(t.live_futures(), 0, "the dead instance's futures are gone");
        assert_eq!(
            t.complete(f, ResultRecord::Ok(vec![])),
            WakeTarget::Orphan,
            "a late completion has no owner"
        );
    }
}
