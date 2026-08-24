//! Instance slots, exclusive call access, and idle pools.
//!
//! [`Slot::enter`] moves the resident instance into a [`CallGuard`]. Another
//! entry into that slot is refused until the guard is dropped. Destroying the
//! guard's instance leaves the slot discarded.
//!
//! [`decide`] keeps parked calls and outstanding background work separate
//! from terminal pool decisions. A successful `fresh` request selects reset;
//! a successful `reuse` request can retain its state. [`InstancePool::release`]
//! applies the disposition and discards instances when reset or pool admission
//! fails.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::config::HandlerLifecycle;
use wisp_engine::{Completion, InstanceOps, ModuleHash};

use crate::bindings::InstanceCtx;
use crate::{InstanceId, ModuleId};

/// Supplies module identity, baseline memory, and instances for the pool.
///
/// Activated instances are `Send` so their ownership can move between workers.
pub trait Activator: Send + Sync + 'static {
    fn module(&self) -> &ModuleId;
    fn module_hash(&self) -> ModuleHash;
    fn abi_rev(&self) -> u32;
    /// Baseline memory bytes exposed to capture and restore callers.
    fn template_image(&self) -> Arc<Vec<u8>>;
    /// Create an instance using the supplied host bindings.
    fn activate(
        &self,
        bindings: Box<dyn wisp_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>>;
}

/// Lifecycle state recorded by a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstanceState {
    Hot,
    Parked,
    Hibernated,
    /// The slot's resident instance has been discarded.
    Discarded,
}

impl InstanceState {
    /// Whether the pair is listed by the lifecycle transition policy.
    pub fn may_transition_to(self, next: Self) -> bool {
        use InstanceState::*;
        matches!(
            (self, next),
            (Hot, Parked)
                | (Hot, Discarded)
                | (Parked, Hot)
                | (Parked, Hibernated)
                | (Parked, Discarded)
                | (Hibernated, Hot)
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            InstanceState::Hot => "hot",
            InstanceState::Parked => "parked",
            InstanceState::Hibernated => "hibernated",
            InstanceState::Discarded => "discarded",
        }
    }
}

/// A lifecycle tier recorded by an instance slot.
pub type Tier = InstanceState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterError {
    /// A call guard already owns the instance.
    Busy,
    /// The slot currently has no resident instance.
    NotResident(InstanceState),
}

impl std::fmt::Display for EnterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnterError::Busy => write!(f, "instance already has a call in flight"),
            EnterError::NotResident(s) => write!(f, "instance is {}", s.label()),
        }
    }
}

/// Exclusive access to a live instance.
///
/// Entry moves the instance out of its slot. Dropping the guard returns it
/// unless [`CallGuard::destroy`] consumed it.
pub struct CallGuard<'a> {
    slot: &'a Slot,
    inst: Option<Box<dyn InstanceOps + Send>>,
}

impl CallGuard<'_> {
    pub fn instance(&mut self) -> &mut (dyn InstanceOps + Send) {
        // Invariant: `inst` is `Some` for the whole life of the guard; it is
        // taken only in `Drop`.
        self.inst
            .as_deref_mut()
            .expect("CallGuard without an instance")
    }
    /// Drop the instance and mark its slot discarded.
    pub fn destroy(mut self) {
        self.inst = None;
        self.slot.set_state(InstanceState::Discarded);
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        let inst = self.inst.take();
        let mut g = self.slot.inner.lock();
        g.in_call = false;
        if let Some(i) = inst {
            g.resident = Some(i);
        } else if g.state != InstanceState::Discarded {
            // The instance was destroyed under us; the slot cannot be Hot with
            // nothing in it.
            g.state = InstanceState::Discarded;
        }
    }
}

struct SlotInner {
    state: InstanceState,
    /// The resident instance between calls; a call guard temporarily owns it.
    resident: Option<Box<dyn InstanceOps + Send>>,
    in_call: bool,
    parked_since_ms: Option<u64>,
    /// Capture key recorded while the slot is hibernated.
    delta_key: Option<String>,
    /// Future IDs recorded by hibernation and drained by `take_shed_futures`.
    shed_on_capture: Vec<u64>,
    /// Set on entry or rehydration and cleared by reset.
    dirty: bool,
}

/// A resident instance, its shared host context, and lifecycle bookkeeping.
pub struct Slot {
    pub id: InstanceId,
    pub ctx: Arc<InstanceCtx>,
    inner: Mutex<SlotInner>,
    /// Milliseconds accrued when entry consumes a recorded park timestamp.
    parked_ms_total: AtomicU64,
}

impl Slot {
    pub fn new(id: InstanceId, ctx: Arc<InstanceCtx>, inst: Box<dyn InstanceOps + Send>) -> Self {
        Self {
            id,
            ctx,
            inner: Mutex::new(SlotInner {
                state: InstanceState::Parked,
                resident: Some(inst),
                in_call: false,
                parked_since_ms: None,
                delta_key: None,
                shed_on_capture: Vec::new(),
                dirty: false,
            }),
            parked_ms_total: AtomicU64::new(0),
        }
    }

    /// Take exclusive ownership of the instance for one call into the guest.
    ///
    /// A second entry into this slot is refused while its guard is alive.
    pub fn enter(&self) -> Result<CallGuard<'_>, EnterError> {
        let mut g = self.inner.lock();
        if g.in_call {
            return Err(EnterError::Busy);
        }
        let Some(inst) = g.resident.take() else {
            return Err(EnterError::NotResident(g.state));
        };
        g.in_call = true;
        g.dirty = true;
        if let Some(since) = g.parked_since_ms.take() {
            let now = crate::now_ms();
            self.parked_ms_total
                .fetch_add(now.saturating_sub(since), Ordering::Relaxed);
        }
        g.state = InstanceState::Hot;
        drop(g);
        Ok(CallGuard {
            slot: self,
            inst: Some(inst),
        })
    }

    pub fn state(&self) -> InstanceState {
        self.inner.lock().state
    }

    pub fn is_busy(&self) -> bool {
        self.inner.lock().in_call
    }

    /// Whether the slot is parked, resident, and has no call in flight.
    pub fn is_capture_eligible(&self) -> bool {
        let g = self.inner.lock();
        !g.in_call && g.state == InstanceState::Parked && g.resident.is_some()
    }

    pub fn is_pristine(&self) -> bool {
        !self.inner.lock().dirty
    }

    pub fn parked_since_ms(&self) -> Option<u64> {
        self.inner.lock().parked_since_ms
    }

    pub fn parked_ms_total(&self) -> u64 {
        self.parked_ms_total.load(Ordering::Relaxed)
    }

    pub fn delta_key(&self) -> Option<String> {
        self.inner.lock().delta_key.clone()
    }

    pub fn take_shed_futures(&self) -> Vec<u64> {
        std::mem::take(&mut self.inner.lock().shed_on_capture)
    }

    pub(crate) fn set_state(&self, s: InstanceState) {
        self.inner.lock().state = s;
    }

    /// Mark a returned guest call parked and record the start of its idle time.
    /// Host resources held by the shared context remain owned by that context.
    pub fn park(&self, now_ms: u64) {
        let mut g = self.inner.lock();
        debug_assert!(
            g.state.may_transition_to(InstanceState::Parked) || g.state == InstanceState::Parked,
            "illegal transition {:?} -> Parked",
            g.state
        );
        g.state = InstanceState::Parked;
        g.parked_since_ms = Some(now_ms);
    }

    /// Drop the resident instance and record its capture key and future IDs.
    /// Refuse while a call is in flight or the state is not parked.
    pub fn hibernate(&self, delta_key: String, shed: Vec<u64>) -> Result<(), HibernateRefused> {
        let mut g = self.inner.lock();
        if g.in_call {
            return Err(HibernateRefused::Hot);
        }
        if g.state != InstanceState::Parked {
            return Err(HibernateRefused::NotParked(g.state));
        }
        g.resident = None;
        g.state = InstanceState::Hibernated;
        g.delta_key = Some(delta_key);
        g.shed_on_capture = shed;
        g.parked_since_ms = None;
        Ok(())
    }

    /// Install the caller's restored instance and return the slot to parked.
    pub fn rehydrate(&self, inst: Box<dyn InstanceOps + Send>) {
        let mut g = self.inner.lock();
        debug_assert_eq!(g.state, InstanceState::Hibernated);
        g.resident = Some(inst);
        g.state = InstanceState::Parked;
        g.delta_key = None;
        g.dirty = true;
    }

    /// Ask the resident instance to reset and clear the slot's dirty marker.
    /// Return an error if a call is in flight or the engine reset fails.
    pub fn reset(&self) -> anyhow::Result<()> {
        let mut g = self.inner.lock();
        if g.in_call {
            anyhow::bail!("cannot reset an instance with a call in flight");
        }
        if let Some(i) = g.resident.as_mut() {
            i.reset()?;
        }
        g.dirty = false;
        g.parked_since_ms = None;
        g.state = InstanceState::Parked;
        Ok(())
    }

    /// Drop the resident instance and clear its capture key and park timestamp.
    /// Resources held by the shared host context are managed separately.
    pub fn discard(&self) {
        let mut g = self.inner.lock();
        g.resident = None;
        g.state = InstanceState::Discarded;
        g.delta_key = None;
        g.parked_since_ms = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HibernateRefused {
    /// A call guard is still in flight.
    Hot,
    NotParked(InstanceState),
}

// ---------------------------------------------------------------------------
// The disposition of an instance after a request.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    /// Keep the instance's state available for a later dispatch.
    Warm,
    /// Reset the instance before admitting it to the reset pool.
    Reset,
    /// Discard: trapped, killed, or evicted.
    Discard,
}

/// The disposition of an instance at the end of a request.
///
/// Policy helpers construct the private value; [`InstancePool::release`]
/// applies it to a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "returning an instance to its pool requires a disposition"]
pub struct Disposition(DispositionKind);

impl Disposition {
    pub fn kind(self) -> DispositionKind {
        self.0
    }
    pub fn is_warm(self) -> bool {
        self.0 == DispositionKind::Warm
    }
    pub fn label(self) -> &'static str {
        match self.0 {
            DispositionKind::Warm => "warm",
            DispositionKind::Reset => "reset",
            DispositionKind::Discard => "discard",
        }
    }
}

/// What happens to the instance when a call into it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestEnd {
    /// The guest parked, so its request remains in flight.
    Parked,
    /// The response completed but background work still postpones disposal.
    RetainedForWaitUntil,
    /// Terminal.
    Disposed(Disposition),
}

/// Select a pending-work result or a terminal pool disposition.
///
/// Traps and budget kills select discard. Once successful work completes,
/// `fresh` selects reset and `reuse` selects warm retention. Pool admission can
/// still discard the instance.
pub fn decide(
    lifecycle: HandlerLifecycle,
    completion: Completion,
    wait_until_outstanding: bool,
) -> RequestEnd {
    match completion {
        Completion::Parked => RequestEnd::Parked,
        Completion::Trapped | Completion::BudgetKill => {
            RequestEnd::Disposed(Disposition(DispositionKind::Discard))
        }
        Completion::Done => {
            if wait_until_outstanding {
                return RequestEnd::RetainedForWaitUntil;
            }
            match lifecycle {
                HandlerLifecycle::Fresh => {
                    RequestEnd::Disposed(Disposition(DispositionKind::Reset))
                }
                HandlerLifecycle::Reuse => RequestEnd::Disposed(Disposition(DispositionKind::Warm)),
            }
        }
    }
}

/// Returns a discard disposition.
pub fn evict() -> Disposition {
    Disposition(DispositionKind::Discard)
}

/// A reset requested by the instance reuse policy.
pub fn policy_reset() -> Disposition {
    Disposition(DispositionKind::Reset)
}

// ---------------------------------------------------------------------------
// Idle pool acquisition.
// ---------------------------------------------------------------------------

/// The pool or activation path that supplied a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireSource {
    /// A slot taken from the warm pool under `reuse`.
    WarmParked,
    /// A slot taken from the reset pool.
    PooledFresh,
    /// An instance constructed by the activator.
    Activated,
}

/// Idle instance pools for one module and lifecycle policy.
pub struct InstancePool {
    pub module: ModuleId,
    pub lifecycle: HandlerLifecycle,
    activator: Arc<dyn Activator>,
    warm: Mutex<Vec<Arc<Slot>>>,
    fresh: Mutex<Vec<Arc<Slot>>>,
    max_idle: usize,
    live: AtomicU64,
}

impl InstancePool {
    pub fn new(
        activator: Arc<dyn Activator>,
        lifecycle: HandlerLifecycle,
        max_idle: usize,
    ) -> Self {
        Self {
            module: activator.module().clone(),
            lifecycle,
            activator,
            warm: Mutex::new(Vec::new()),
            fresh: Mutex::new(Vec::new()),
            max_idle,
            live: AtomicU64::new(0),
        }
    }

    pub fn activator(&self) -> &Arc<dyn Activator> {
        &self.activator
    }

    pub fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }

    pub fn idle_counts(&self) -> (usize, usize) {
        (self.warm.lock().len(), self.fresh.lock().len())
    }

    /// Try the warm pool for `reuse`, then the reset pool, then activation.
    /// A `fresh` acquisition skips the warm pool.
    pub fn acquire<F>(&self, make_ctx: F) -> anyhow::Result<(Arc<Slot>, AcquireSource)>
    where
        F: FnOnce(InstanceId) -> Arc<InstanceCtx>,
    {
        if self.lifecycle == HandlerLifecycle::Reuse {
            if let Some(s) = self.warm.lock().pop() {
                return Ok((s, AcquireSource::WarmParked));
            }
        }
        if let Some(s) = self.fresh.lock().pop() {
            debug_assert!(s.is_pristine(), "a pooled-fresh instance carried residue");
            return Ok((s, AcquireSource::PooledFresh));
        }
        let id = InstanceId::alloc();
        let ctx = make_ctx(id);
        let bindings = crate::bindings::CellBindings::boxed(ctx.clone());
        let inst = self.activator.activate(bindings)?;
        self.live.fetch_add(1, Ordering::Relaxed);
        Ok((Arc::new(Slot::new(id, ctx, inst)), AcquireSource::Activated))
    }

    /// Apply a disposition, discarding if reset fails or an idle pool is full.
    pub fn release(&self, slot: Arc<Slot>, d: Disposition) {
        match d.kind() {
            DispositionKind::Warm => {
                debug_assert_ne!(
                    self.lifecycle,
                    HandlerLifecycle::Fresh,
                    "a `fresh` instance may never be warmed"
                );
                if self.lifecycle == HandlerLifecycle::Fresh {
                    // Fresh reuse requires reset even if a caller supplies Warm.
                    return self.release(slot, policy_reset());
                }
                slot.park(crate::now_ms());
                let mut w = self.warm.lock();
                if w.len() < self.max_idle {
                    w.push(slot);
                } else {
                    drop(w);
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                }
            }
            DispositionKind::Reset => {
                if slot.reset().is_err() {
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                let mut f = self.fresh.lock();
                if f.len() < self.max_idle {
                    f.push(slot);
                } else {
                    drop(f);
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                }
            }
            DispositionKind::Discard => {
                slot.discard();
                self.live.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Discard all idle entries and return the number removed.
    pub fn drain_idle(&self) -> usize {
        let mut n = 0;
        for s in self
            .warm
            .lock()
            .drain(..)
            .chain(self.fresh.lock().drain(..))
        {
            s.discard();
            self.live.fetch_sub(1, Ordering::Relaxed);
            n += 1;
        }
        n
    }

    /// Clone the current idle-pool slot references for inspection.
    pub fn idle_parked(&self) -> Vec<Arc<Slot>> {
        let mut v = self.warm.lock().clone();
        v.extend(self.fresh.lock().iter().cloned());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use wisp_abi::config::Limits;

    fn ctx_for(id: InstanceId, lifecycle: HandlerLifecycle) -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::for_test(id, lifecycle, Limits::default()))
    }

    fn pool(lifecycle: HandlerLifecycle) -> InstancePool {
        InstancePool::new(
            Arc::new(FakeActivator::new(FakeBehaviour::default())),
            lifecycle,
            16,
        )
    }

    /// Check the listed transition pairs, including no exit from Discarded.
    #[test]
    fn lifecycle_transitions_match_the_spec_table() {
        use InstanceState::*;
        let legal = [
            (Hot, Parked),
            (Hot, Discarded),
            (Parked, Hot),
            (Parked, Hibernated),
            (Parked, Discarded),
            (Hibernated, Hot),
        ];
        for a in [Hot, Parked, Hibernated, Discarded] {
            for b in [Hot, Parked, Hibernated, Discarded] {
                assert_eq!(
                    a.may_transition_to(b),
                    legal.contains(&(a, b)),
                    "{a:?} -> {b:?}"
                );
            }
        }
        assert!(!Hibernated.may_transition_to(Parked));
        assert!(!Discarded.may_transition_to(Hot));
    }

    /// The second entry is refused while the first guard owns the instance.
    #[test]
    fn a_second_call_into_a_busy_instance_is_impossible() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        let guard = slot.enter().expect("first entry");
        assert_eq!(slot.enter().err(), Some(EnterError::Busy));
        assert_eq!(slot.state(), InstanceState::Hot);
        drop(guard);
        // Once the call returns, the instance is available again.
        assert!(slot.enter().is_ok());
    }

    #[test]
    fn a_hibernated_slot_has_nothing_to_call_into() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        slot.park(0);
        slot.hibernate("k".into(), vec![]).unwrap();
        assert_eq!(
            slot.enter().err(),
            Some(EnterError::NotResident(InstanceState::Hibernated))
        );
    }

    /// Capture eligibility requires a parked slot with no call in flight.
    #[test]
    fn capture_is_refused_unless_parked() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        let guard = slot.enter().unwrap();
        assert!(!slot.is_capture_eligible());
        assert_eq!(
            slot.hibernate("k".into(), vec![]),
            Err(HibernateRefused::Hot)
        );
        drop(guard);
        slot.park(0);
        assert!(slot.is_capture_eligible());
        assert!(slot.hibernate("k".into(), vec![]).is_ok());
    }

    /// Fresh completion selects reset or discard after pending work ends.
    #[test]
    fn fresh_never_decides_warm() {
        use Completion::*;
        for c in [Done, Parked, Trapped, BudgetKill] {
            for wait in [false, true] {
                let end = decide(HandlerLifecycle::Fresh, c, wait);
                if let RequestEnd::Disposed(d) = end {
                    assert!(!d.is_warm(), "fresh produced Warm for {c:?} wait={wait}");
                }
            }
        }
        // And the specific rows.
        assert_eq!(
            decide(HandlerLifecycle::Fresh, Done, false),
            RequestEnd::Disposed(Disposition(DispositionKind::Reset))
        );
        assert_eq!(
            decide(HandlerLifecycle::Fresh, Done, true),
            RequestEnd::RetainedForWaitUntil,
            "reset waits for outstanding background work"
        );
        assert_eq!(
            decide(HandlerLifecycle::Reuse, Done, false),
            RequestEnd::Disposed(Disposition(DispositionKind::Warm))
        );
        for lc in [HandlerLifecycle::Fresh, HandlerLifecycle::Reuse] {
            assert_eq!(
                decide(lc, BudgetKill, false),
                RequestEnd::Disposed(Disposition(DispositionKind::Discard)),
                "a budget-killed instance is discarded"
            );
            assert_eq!(
                decide(lc, Trapped, false),
                RequestEnd::Disposed(Disposition(DispositionKind::Discard))
            );
            assert_eq!(decide(lc, Parked, false), RequestEnd::Parked);
        }
    }

    /// Fresh acquisition reuses a reset slot without consulting the warm pool.
    #[test]
    fn fresh_never_touches_the_warm_pool() {
        let p = pool(HandlerLifecycle::Fresh);
        let (a, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .unwrap();
        assert_eq!(src, AcquireSource::Activated);
        let id_a = a.id;
        // Finish a request the way the cell does.
        let RequestEnd::Disposed(d) = decide(HandlerLifecycle::Fresh, Completion::Done, false)
        else {
            panic!("expected a terminal disposition");
        };
        p.release(a, d);
        let (warm, fresh) = p.idle_counts();
        assert_eq!(warm, 0, "a fresh app must never populate the warm pool");
        assert_eq!(fresh, 1);

        // Successful reset keeps the slot available in the reset pool.
        let (b, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .unwrap();
        assert_eq!(src, AcquireSource::PooledFresh);
        assert_eq!(b.id, id_a);
        assert!(b.is_pristine());
    }

    #[test]
    fn reuse_prefers_warm_then_fresh_then_activate() {
        let p = pool(HandlerLifecycle::Reuse);
        let (a, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        assert_eq!(src, AcquireSource::Activated);
        let (b, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        let warm_id = a.id;
        let fresh_id = b.id;
        p.release(a, Disposition(DispositionKind::Warm));
        p.release(b, policy_reset());

        let (got, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        assert_eq!(src, AcquireSource::WarmParked);
        assert_eq!(got.id, warm_id);

        let (got, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        assert_eq!(src, AcquireSource::PooledFresh);
        assert_eq!(got.id, fresh_id);

        let (_, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        assert_eq!(src, AcquireSource::Activated);
    }

    /// A budget-killed instance is removed from both idle pools.
    #[test]
    fn a_killed_instance_leaves_the_pool_entirely() {
        let p = pool(HandlerLifecycle::Reuse);
        let (a, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .unwrap();
        let RequestEnd::Disposed(d) =
            decide(HandlerLifecycle::Reuse, Completion::BudgetKill, false)
        else {
            panic!()
        };
        p.release(a, d);
        assert_eq!(p.idle_counts(), (0, 0));
        assert_eq!(p.live(), 0);
    }
}
