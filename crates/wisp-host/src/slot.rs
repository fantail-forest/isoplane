//! Instance slots with exclusive call access through [`CallGuard`].

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_engine::InstanceOps;

use crate::bindings::InstanceCtx;
use crate::InstanceId;

/// Lifecycle state shared with the host interface.
pub use wisp_abi::config::LifecycleTier;

/// The byte encoding for `state_hint`; private so the order never becomes
/// contract surface.
fn tier_as_u8(t: LifecycleTier) -> u8 {
    match t {
        LifecycleTier::Hot => 0,
        LifecycleTier::Parked => 1,
        LifecycleTier::Hibernated => 2,
        LifecycleTier::Discarded => 3,
    }
}

fn tier_from_u8(v: u8) -> LifecycleTier {
    match v {
        0 => LifecycleTier::Hot,
        1 => LifecycleTier::Parked,
        2 => LifecycleTier::Hibernated,
        _ => LifecycleTier::Discarded,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterError {
    /// A previous call into this instance has not returned.
    Busy,
    /// Hibernated or discarded: there is no memory here to call into.
    NotResident(LifecycleTier),
    /// The instance was hard-killed or trapped, so its memory may be
    /// mid-collection or mid-frame. wasmtime would still answer a call on the
    /// killed store, so the refusal is the platform's to make.
    Tombstoned,
}

impl std::fmt::Display for EnterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnterError::Busy => write!(f, "reentrant call refused"),
            EnterError::NotResident(s) => write!(f, "instance is {}", s.as_str()),
            EnterError::Tombstoned => write!(
                f,
                "instance is a tombstone: it was hard-killed or trapped and refuses further calls"
            ),
        }
    }
}

/// Exclusive access to a live instance. Dropping the guard returns it unless
/// `CallGuard::destroy` consumed it.
pub struct CallGuard<'a> {
    slot: &'a Slot,
    /// `Some` until `destroy` or `Drop` takes it, and neither can be followed
    /// by a call to `instance`.
    inst: Option<Box<dyn InstanceOps + Send>>,
}

impl CallGuard<'_> {
    pub fn instance(&mut self) -> &mut (dyn InstanceOps + Send) {
        self.inst
            .as_deref_mut()
            .expect("a CallGuard holds its instance until destroy or Drop consumes it")
    }
    /// Terminates the instance instead of returning it, for traps and hard
    /// kills. The tombstone is set while the guard is still held, so `Drop`
    /// clears `in_call` on a slot that already refuses; a plain drop followed
    /// by a tombstone would leave an instant in which another thread could
    /// enter memory a trap abandoned.
    pub fn destroy(mut self) {
        self.inst = None;
        self.slot.tombstone();
        self.slot.set_state(LifecycleTier::Discarded);
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        let inst = self.inst.take();
        let mut g = self.slot.inner.lock();
        g.in_call = false;
        match inst {
            Some(_) if std::thread::panicking() => {
                g.tombstoned = true;
                g.state = LifecycleTier::Discarded;
                g.resident = None;
            }
            Some(i) => {
                g.resident = Some(i);
                // Back to the tier it came from, so a host-initiated call such
                // as the pre-capture gc hint leaves no trace.
                g.state = g.resume_state;
                if g.state == LifecycleTier::Parked && g.parked_since_ms.is_none() {
                    g.parked_since_ms = Some(crate::now_ms().max(1));
                }
            }
            None => {
                g.resident = None;
                g.state = LifecycleTier::Discarded;
            }
        }
        self.slot.publish(&g);
    }
}

struct SlotInner {
    state: LifecycleTier,
    /// The tier to return to when the current call returns. Entering makes an
    /// instance momentarily Hot, including for the pre-capture gc hint, which
    /// would otherwise leave the slot Hot and fail the capture's own check.
    resume_state: LifecycleTier,
    /// The resident instance between calls; a call guard temporarily owns it.
    resident: Option<Box<dyn InstanceOps + Send>>,
    in_call: bool,
    parked_since_ms: Option<u64>,
    /// Key of this instance's delta in the hibernation store, when sleeping.
    delta_key: Option<String>,
    /// Ids shed at capture, owed to the guest as `FutureLost` on wake.
    shed_on_capture: Vec<u64>,
    /// Set on entry or rehydration and cleared by reset.
    dirty: bool,
    /// Once set, [`Slot::enter`] refuses. [`CallGuard::destroy`] sets it while
    /// the guard is held; a site that drops the guard after a trap and
    /// tombstones later leaves an instant in which another thread can enter.
    tombstoned: bool,
}

pub struct Slot {
    pub id: InstanceId,
    pub ctx: Arc<InstanceCtx>,
    inner: Mutex<SlotInner>,
    execution: Mutex<()>,
    /// Accumulated parked time, reported separately from active execution time.
    parked_ms_total: AtomicU64,
    /// Lock-free mirrors of `inner.state` and `inner.parked_since_ms`, written
    /// in the same critical sections, so the idle sweep and the gauges do not
    /// take a mutex per parked instance per tick.
    state_hint: AtomicU8,
    parked_since_hint: AtomicU64,
}

impl Slot {
    pub fn new(id: InstanceId, ctx: Arc<InstanceCtx>, inst: Box<dyn InstanceOps + Send>) -> Self {
        Self {
            id,
            ctx,
            inner: Mutex::new(SlotInner {
                state: LifecycleTier::Parked,
                resume_state: LifecycleTier::Parked,
                resident: Some(inst),
                in_call: false,
                parked_since_ms: None,
                delta_key: None,
                shed_on_capture: Vec::new(),
                dirty: false,
                tombstoned: false,
            }),
            execution: Mutex::new(()),
            parked_ms_total: AtomicU64::new(0),
            state_hint: AtomicU8::new(tier_as_u8(LifecycleTier::Parked)),
            parked_since_hint: AtomicU64::new(0),
        }
    }

    fn publish(&self, g: &SlotInner) {
        self.state_hint
            .store(tier_as_u8(g.state), Ordering::Relaxed);
        self.parked_since_hint
            .store(g.parked_since_ms.unwrap_or(0), Ordering::Relaxed);
    }

    /// Serializes a dispatch slice with its settlement and pool release, so
    /// a queued wake cannot enter between a guest return and its reset.
    pub(crate) fn lock_execution(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.execution.lock()
    }

    /// Acquires idle lifecycle ownership without waiting for a running dispatch.
    pub(crate) fn try_lock_execution(&self) -> Option<parking_lot::MutexGuard<'_, ()>> {
        self.execution.try_lock()
    }

    /// Take exclusive ownership of this slot's instance for a guest call.
    pub fn enter(&self) -> Result<CallGuard<'_>, EnterError> {
        let mut g = self.inner.lock();
        if g.tombstoned {
            return Err(EnterError::Tombstoned);
        }
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
        g.resume_state = g.state;
        g.state = LifecycleTier::Hot;
        self.publish(&g);
        drop(g);
        Ok(CallGuard {
            slot: self,
            inst: Some(inst),
        })
    }

    pub fn state(&self) -> LifecycleTier {
        tier_from_u8(self.state_hint.load(Ordering::Relaxed))
    }

    /// Capture is eligible only when Parked and never from a tombstone.
    pub fn is_capture_eligible(&self) -> bool {
        let g = self.inner.lock();
        !g.tombstoned && !g.in_call && g.state == LifecycleTier::Parked && g.resident.is_some()
    }

    /// Mark this slot as discarded, refusing further guest calls.
    pub fn tombstone(&self) {
        let mut g = self.inner.lock();
        g.tombstoned = true;
        self.ctx.end_http_dispatch();
    }

    pub fn is_tombstoned(&self) -> bool {
        self.inner.lock().tombstoned
    }

    pub fn is_pristine(&self) -> bool {
        !self.inner.lock().dirty
    }

    pub fn parked_since_ms(&self) -> Option<u64> {
        match self.parked_since_hint.load(Ordering::Relaxed) {
            0 => None,
            v => Some(v),
        }
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

    pub(crate) fn set_state(&self, s: LifecycleTier) {
        let mut g = self.inner.lock();
        g.state = s;
        self.publish(&g);
    }

    pub fn park(&self, now_ms: u64) {
        let mut g = self.inner.lock();
        debug_assert!(
            g.state.may_transition_to(LifecycleTier::Parked) || g.state == LifecycleTier::Parked,
            "illegal transition {:?} -> Parked",
            g.state
        );
        g.state = LifecycleTier::Parked;
        g.parked_since_ms = Some(now_ms.max(1));
        self.publish(&g);
    }

    /// Drop the resident instance and record its capture key and future IDs.
    /// Refuse while a call is in flight, the slot is tombstoned, or it is not parked.
    pub fn hibernate(&self, delta_key: String, shed: Vec<u64>) -> Result<(), HibernateRefused> {
        let mut g = self.inner.lock();
        if g.tombstoned {
            return Err(HibernateRefused::Tombstoned);
        }
        if g.in_call {
            return Err(HibernateRefused::Hot);
        }
        if g.state != LifecycleTier::Parked {
            return Err(HibernateRefused::NotParked(g.state));
        }
        g.resident = None;
        g.state = LifecycleTier::Hibernated;
        g.delta_key = Some(delta_key);
        g.shed_on_capture = shed;
        g.parked_since_ms = None;
        self.publish(&g);
        Ok(())
    }

    /// Install the caller's restored instance and return the slot to parked.
    pub fn rehydrate(&self, inst: Box<dyn InstanceOps + Send>) {
        let mut g = self.inner.lock();
        debug_assert!(!g.tombstoned, "a tombstone cannot be rehydrated");
        debug_assert_eq!(g.state, LifecycleTier::Hibernated);
        g.resident = Some(inst);
        g.state = LifecycleTier::Parked;
        g.delta_key = None;
        g.dirty = true;
        self.publish(&g);
    }

    /// Reset to template contents. Refused for a tombstone, because an `Ok`
    /// here is what pool acquisition takes as "reusable".
    pub fn reset(&self) -> anyhow::Result<()> {
        let mut g = self.inner.lock();
        if g.tombstoned {
            anyhow::bail!(
                "refusing to reset a tombstoned slot: a hard-killed instance is discarded"
            );
        }
        if g.in_call {
            anyhow::bail!("cannot reset an instance with a call in flight");
        }
        if let Some(i) = g.resident.as_mut() {
            i.reset()?;
        }
        self.ctx.release_native_resources();
        g.dirty = false;
        g.parked_since_ms = None;
        g.state = LifecycleTier::Parked;
        self.publish(&g);
        Ok(())
    }

    pub fn discard(&self) {
        let mut g = self.inner.lock();
        g.resident = None;
        self.ctx.release_native_resources();
        g.state = LifecycleTier::Discarded;
        g.delta_key = None;
        g.parked_since_ms = None;
        self.publish(&g);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HibernateRefused {
    Tombstoned,
    /// A call is in flight, so the stack is not empty.
    Hot,
    NotParked(LifecycleTier),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use crate::pool::InstancePool;
    use wisp_abi::config::{HandlerLifecycle, Limits};

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

    fn pollable(slot: &Slot) -> wisp_abi::native_io::ResourceHandle {
        use wisp_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        match bindings
            .native_io(wisp_abi::native_io::Request::Timer(
                wisp_abi::native_io::DurationNanos(0),
            ))
            .expect("the slot must allocate an instance-owned pollable")
        {
            wisp_abi::native_io::Response::Handle(handle) => handle,
            response => panic!("pollable allocation returned an unexpected result: {response:?}"),
        }
    }

    fn assert_pollable_released(slot: &Slot, handle: wisp_abi::native_io::ResourceHandle) {
        use wisp_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        assert!(
            matches!(bindings.native_io(wisp_abi::native_io::Request::Ready(handle)),
            Err(wisp_abi::native_io::Error::InvalidHandle(received)) if received == handle),
            "a released slot must not retain a previous native pollable handle"
        );
    }

    #[test]
    fn resetting_to_pristine_memory_releases_native_resources() {
        let pool = pool(HandlerLifecycle::Reuse);
        let (slot, _) = pool
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("the slot must activate");
        let handle = pollable(&slot);
        slot.reset()
            .expect("the idle slot must reset to its template");
        assert_pollable_released(&slot, handle);
    }

    #[test]
    fn discarding_a_slot_releases_native_resources_even_while_its_context_is_retained() {
        let pool = pool(HandlerLifecycle::Reuse);
        let (slot, _) = pool
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("the slot must activate");
        let handle = pollable(&slot);
        slot.discard();
        assert_pollable_released(&slot, handle);
    }

    #[test]
    fn lifecycle_transitions_match_the_spec_table() {
        use LifecycleTier::*;
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
                    "{a:?} -> {b:?} disagrees with the tier table"
                );
            }
        }
        assert!(
            !Hibernated.may_transition_to(Parked),
            "a wake makes an instance Hot, never Parked"
        );
        assert!(
            !Discarded.may_transition_to(Hot),
            "the only exit from Discarded is a fresh activation, a new slot"
        );
    }

    #[test]
    fn a_second_call_into_a_busy_instance_is_impossible() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        let guard = slot.enter().expect("first entry");
        assert_eq!(
            slot.enter().err(),
            Some(EnterError::Busy),
            "the first entry holds the instance"
        );
        assert_eq!(slot.state(), LifecycleTier::Hot, "an entered slot is Hot");
        drop(guard);
        assert!(
            slot.enter().is_ok(),
            "the instance is available again once the call returns"
        );
    }

    #[test]
    fn a_hibernated_slot_has_nothing_to_call_into() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        slot.park(0);
        slot.hibernate("k".into(), vec![]).expect("hibernate");
        assert_eq!(
            slot.enter().err(),
            Some(EnterError::NotResident(LifecycleTier::Hibernated)),
            "a hibernated slot has no memory to call into"
        );
    }

    #[test]
    fn capture_is_refused_unless_parked() {
        let p = pool(HandlerLifecycle::Reuse);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        let guard = slot.enter().expect("enter");
        assert!(
            !slot.is_capture_eligible(),
            "a slot with a call in flight is not capturable"
        );
        assert_eq!(
            slot.hibernate("k".into(), vec![]),
            Err(HibernateRefused::Hot),
            "hibernating a Hot slot is refused"
        );
        drop(guard);
        slot.park(0);
        assert!(slot.is_capture_eligible(), "a parked slot is capturable");
        assert!(
            slot.hibernate("k".into(), vec![]).is_ok(),
            "hibernating a parked slot succeeds"
        );
    }
}
