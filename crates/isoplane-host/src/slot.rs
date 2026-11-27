//! Instance slots with exclusive call access through [`CallGuard`].

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use isoplane_engine::InstanceOps;
use parking_lot::Mutex;

use crate::bindings::InstanceCtx;
use crate::InstanceId;

/// Lifecycle state shared with the host interface.
pub use isoplane_abi::config::LifecycleTier;

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
    /// Hibernated or discarded: there is no memory here to call into. A
    /// killed or trapped instance is discarded, because its memory may be
    /// mid-collection or mid-frame and wasmtime would still answer a call on
    /// the killed store.
    NotResident(LifecycleTier),
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
    /// kills. The slot is discarded while the guard is still held, so `Drop`
    /// clears `in_call` on a slot that already refuses; a plain drop followed
    /// by a discard would leave an instant in which another thread could
    /// enter memory a trap abandoned.
    pub fn destroy(mut self) {
        self.inst = None;
        self.slot.discard();
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        let inst = self.inst.take();
        let mut g = self.slot.inner.lock();
        g.in_call = false;
        match inst {
            Some(_) if std::thread::panicking() => {
                g.state = LifecycleTier::Discarded;
                g.resident = None;
                self.slot.ctx.snapshot.lock().take();
            }
            // Discarded under this call by another owner, such as a stopping
            // cell: nothing leaves Discarded, so the instance is dropped.
            Some(_) if g.state == LifecycleTier::Discarded => {}
            Some(i) => {
                g.resident = Some(i);
                // Back to the tier it came from, so a host-initiated call such
                // as the pre-capture gc hint leaves no trace.
                g.state = g.resume_state;
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
    dormant: Option<Box<dyn isoplane_engine::DormantInstance>>,
    in_call: bool,
}

pub(crate) struct WakeReservation(Arc<Slot>);

impl Drop for WakeReservation {
    fn drop(&mut self) {
        self.0.wake_queued.store(false, Ordering::Release);
    }
}

pub struct Slot {
    pub id: InstanceId,
    pub ctx: Arc<InstanceCtx>,
    inner: Mutex<SlotInner>,
    execution: Mutex<()>,
    wake_queued: std::sync::atomic::AtomicBool,

    /// Lock-free mirror of `inner.state`, written in the same critical
    /// sections, so the idle sweep and the gauges do not take a mutex per
    /// parked instance per tick.
    state_hint: AtomicU8,

    last_used_ms: AtomicU64,
    memory_bytes: AtomicU64,
    parks: AtomicU64,
    /// Set once the instance's memory has passed its limit: it takes no new
    /// dispatches and is discarded when its last one ends.
    retiring: std::sync::atomic::AtomicBool,
}

impl Slot {
    pub fn new(id: InstanceId, ctx: Arc<InstanceCtx>, inst: Box<dyn InstanceOps + Send>) -> Self {
        let memory_bytes = inst.memory_len() as u64;
        Self {
            id,
            ctx,
            inner: Mutex::new(SlotInner {
                state: LifecycleTier::Parked,
                resume_state: LifecycleTier::Parked,
                resident: Some(inst),
                dormant: None,
                in_call: false,
            }),
            execution: Mutex::new(()),
            wake_queued: std::sync::atomic::AtomicBool::new(false),

            state_hint: AtomicU8::new(tier_as_u8(LifecycleTier::Parked)),

            last_used_ms: AtomicU64::new(0),
            memory_bytes: AtomicU64::new(memory_bytes),
            parks: AtomicU64::new(0),
            retiring: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Coalesces notifications without consuming their separately stored results.
    pub(crate) fn reserve_wake(self: &Arc<Self>) -> Option<WakeReservation> {
        self.wake_queued
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| WakeReservation(self.clone()))
    }

    fn publish(&self, g: &SlotInner) {
        if let Some(instance) = &g.resident {
            self.memory_bytes
                .store(instance.memory_len() as u64, Ordering::Relaxed);
        }
        self.state_hint
            .store(tier_as_u8(g.state), Ordering::Relaxed);
    }

    /// Serializes a dispatch slice with its settlement and pool release, so
    /// a queued wake cannot enter between a guest return and its release.
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
        if g.in_call {
            return Err(EnterError::Busy);
        }
        let Some(inst) = g.resident.take() else {
            return Err(EnterError::NotResident(g.state));
        };
        g.in_call = true;

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

    /// Most recent request arrival, retained while the instance is hibernated.
    pub fn last_used_ms(&self) -> u64 {
        self.last_used_ms.load(Ordering::Relaxed)
    }

    /// Uses the last empty-stack reading; admitted work may still grow memory.
    pub(crate) fn has_memory_for_join(&self) -> bool {
        self.memory_bytes.load(Ordering::Relaxed) < self.ctx.limits.instance_memory_max
    }

    /// Retires the instance; true if it was not already retiring.
    pub(crate) fn retire(&self) -> bool {
        !self.retiring.swap(true, Ordering::Relaxed)
    }

    pub fn is_retiring(&self) -> bool {
        self.retiring.load(Ordering::Relaxed)
    }

    /// Number of empty-stack boundaries; changes even without new request demand.
    pub fn parks(&self) -> u64 {
        self.parks.load(Ordering::Relaxed)
    }

    /// Capture is eligible only when Parked.
    pub fn is_capture_eligible(&self) -> bool {
        let g = self.inner.lock();
        !g.in_call && g.state == LifecycleTier::Parked && g.resident.is_some()
    }

    /// Records demand from requests without treating other dispatches as demand.
    pub(crate) fn record_dispatch(&self, record: &isoplane_abi::DispatchRecord) {
        match record.kind {
            isoplane_abi::DispatchKind::Http(_) => {
                self.last_used_ms
                    .fetch_max(record.now_ms, Ordering::Relaxed);
            }
            isoplane_abi::DispatchKind::Background => {}
        }
    }

    fn refused(&self, from: LifecycleTier, to: LifecycleTier) -> TransitionError {
        TransitionError {
            instance: self.id,
            from,
            to,
        }
    }

    /// Records an empty-stack boundary on a resident instance. Refused, with
    /// the slot unchanged, for an instance that holds no memory: a hibernated
    /// one returns through [`Slot::rehydrate`] and a discarded one never does.
    /// Refused too while a call is in flight, whose [`CallGuard`] owns the
    /// instance until it drops and returns the slot to its tier itself.
    pub fn park(&self) -> Result<(), TransitionError> {
        let mut g = self.inner.lock();
        if !g.state.is_resident() || g.in_call {
            return Err(self.refused(g.state, LifecycleTier::Parked));
        }
        self.parks.fetch_add(1, Ordering::Relaxed);
        g.state = LifecycleTier::Parked;
        self.publish(&g);
        Ok(())
    }

    /// Releases guest pages after saving a local snapshot, retaining mappings
    /// when supported. Refused, with the slot unchanged, unless the instance
    /// is Parked with no call in flight.
    pub fn hibernate(&self) -> Result<(), TransitionError> {
        let mut g = self.inner.lock();
        if !g.state.is_resident() || g.in_call {
            return Err(self.refused(g.state, LifecycleTier::Hibernated));
        }
        g.dormant = g.resident.take().and_then(|instance| instance.evict());
        g.state = LifecycleTier::Hibernated;
        self.publish(&g);
        Ok(())
    }

    pub(crate) fn take_dormant(&self) -> Option<Box<dyn isoplane_engine::DormantInstance>> {
        let mut g = self.inner.lock();
        if g.state == LifecycleTier::Hibernated {
            g.dormant.take()
        } else {
            None
        }
    }

    /// Installs the caller's restored instance and returns the slot to
    /// Parked. Refused, dropping `inst` and leaving the slot unchanged, unless
    /// the slot is Hibernated.
    pub fn rehydrate(&self, inst: Box<dyn InstanceOps + Send>) -> Result<(), TransitionError> {
        let mut g = self.inner.lock();
        if g.state != LifecycleTier::Hibernated {
            return Err(self.refused(g.state, LifecycleTier::Parked));
        }
        g.resident = Some(inst);
        g.state = LifecycleTier::Parked;
        g.dormant = None;
        self.publish(&g);
        Ok(())
    }

    /// Ends the instance and releases what the host holds for it. Every tier
    /// may be discarded and discarding again changes nothing, so this is the
    /// one transition that is never refused.
    pub fn discard(&self) {
        let mut g = self.inner.lock();
        g.resident = None;
        g.dormant = None;
        self.ctx.snapshot.lock().take();
        self.ctx.release_native_resources();
        self.ctx.release_futures();
        self.ctx.admission.lock().take();
        g.state = LifecycleTier::Discarded;
        self.publish(&g);
    }
}

/// A lifecycle transition the slot refused; the slot is unchanged. A call
/// in flight refuses from [`LifecycleTier::Hot`], the tier entering it set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    /// The instance that refused.
    pub instance: InstanceId,
    /// The tier the instance was in.
    pub from: LifecycleTier,
    /// The tier that was asked for.
    pub to: LifecycleTier,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { instance, from, to } = self;
        write!(f, "instance {instance} cannot become {to} from {from}")
    }
}

impl std::error::Error for TransitionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use crate::pool::InstancePool;

    fn ctx_for(id: InstanceId) -> Arc<InstanceCtx> {
        Arc::new(crate::fakes::instance_ctx(
            id,
            isoplane_abi::config::Limits::default(),
        ))
    }

    fn pool() -> InstancePool {
        InstancePool::new(Arc::new(FakeActivator::new(FakeBehaviour::default())), 16)
    }

    fn pollable(slot: &Slot) -> isoplane_abi::native_io::ResourceHandle {
        use isoplane_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        match bindings
            .native_io(isoplane_abi::native_io::Request::Timer(
                isoplane_abi::native_io::DurationNanos(0),
            ))
            .expect("the slot must allocate an instance-owned pollable")
        {
            isoplane_abi::native_io::Response::Handle(handle) => handle,
            response => panic!("pollable allocation returned an unexpected result: {response:?}"),
        }
    }

    fn assert_pollable_released(slot: &Slot, handle: isoplane_abi::native_io::ResourceHandle) {
        use isoplane_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        assert!(
            matches!(bindings.native_io(isoplane_abi::native_io::Request::Ready(handle)),
            Err(isoplane_abi::native_io::Error::InvalidHandle(received)) if received == handle),
            "a released slot must not retain a previous native pollable handle"
        );
    }

    fn prepared_native_instance() -> (
        isoplane_engine::IsoplaneEngine,
        isoplane_engine::Template,
        Arc<InstanceCtx>,
        isoplane_engine::Instance,
    ) {
        let prepared = isoplane_abi::native_io::PreparedResources {
            ready_pollables: vec![isoplane_abi::native_io::ResourceHandle(4)],
            pending_timers: Vec::new(),

            network: Some(isoplane_abi::native_io::ResourceHandle(3)),
            next_handle: 5,
        };
        let bytes = isoplane_test_support::wat_bytes(&postcard::to_allocvec(&prepared).unwrap());
        let wasm = wat::parse_str(format!(r#"(module
          (@custom "{}" "{bytes}")
          (import "wasi:sockets/instance-network@0.2.0" "instance-network" (func $network (result i32)))
          (memory (export "memory") 1 64)
          (func (export "isoplane_gc_hint") (param i32)
            (i32.store (i32.const 64) (call $network))
            (if (i32.ne (i32.load (i32.const 64)) (i32.const 3)) (then unreachable)))
          (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
          (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
          (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#, isoplane_abi::native_io::PREPARED_RESOURCES_SECTION)).unwrap();
        let engine = isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
            max_instances: 4,
            max_memory_bytes: 4 << 20,
            guard_size_bytes: 2 << 20,
            ..Default::default()
        })
        .unwrap();
        let template = engine.load_template(&wasm).unwrap();
        let ctx = ctx_for(InstanceId(1));
        let instance = engine
            .activate(&template, crate::bindings::CellBindings::boxed(ctx.clone()))
            .unwrap();
        (engine, template, ctx, instance)
    }

    #[test]
    fn replacing_a_store_preserves_retained_native_resources_and_allocation_progress() {
        let (engine, template, ctx, instance) = prepared_native_instance();
        let slot = Slot::new(ctx.instance, ctx.clone(), Box::new(instance));
        let retained = pollable(&slot);
        let replacement = engine
            .activate(&template, crate::bindings::CellBindings::boxed(ctx.clone()))
            .expect("a hibernation wake must bind the already-owned native state");
        use isoplane_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(ctx);
        assert!(
            matches!(
                bindings.native_io(isoplane_abi::native_io::Request::Ready(retained)),
                Ok(isoplane_abi::native_io::Response::Ready(true))
            ),
            "replacing a Store must preserve the retained pollable"
        );
        assert_eq!(
            pollable(&slot).0,
            retained.0 + 1,
            "a replacement Store must not rewind native handle allocation"
        );
        drop(replacement);
    }

    #[test]
    fn discarding_a_slot_releases_native_resources_even_while_its_context_is_retained() {
        let pool = pool();
        let (slot, _) = pool.acquire(ctx_for).expect("the slot must activate");
        let handle = pollable(&slot);
        slot.discard();
        assert_pollable_released(&slot, handle);
    }

    /// A slot in `tier`. A Hot one is returned Parked: only the guard its
    /// caller takes by entering it keeps it Hot.
    fn slot_in(pool: &InstancePool, tier: LifecycleTier) -> Arc<Slot> {
        let (slot, _) = pool.acquire(ctx_for).expect("acquire");
        match tier {
            LifecycleTier::Parked | LifecycleTier::Hot => {}
            LifecycleTier::Hibernated => slot.hibernate().expect("a parked slot hibernates"),
            LifecycleTier::Discarded => slot.discard(),
        }
        slot
    }

    fn fresh_instance(slot: &Slot) -> Box<dyn InstanceOps + Send> {
        use crate::pool::Activator;
        FakeActivator::new(FakeBehaviour::default())
            .activate(crate::bindings::CellBindings::boxed(slot.ctx.clone()))
            .expect("the fake activates")
    }

    #[test]
    fn only_a_hibernated_instance_is_restored() {
        let pool = pool();
        let parked = slot_in(&pool, LifecycleTier::Parked);
        assert_eq!(
            parked.rehydrate(fresh_instance(&parked)),
            Err(TransitionError {
                instance: parked.id,
                from: LifecycleTier::Parked,
                to: LifecycleTier::Parked,
            }),
            "a resident instance has nothing to restore"
        );

        let hibernated = slot_in(&pool, LifecycleTier::Hibernated);
        assert_eq!(hibernated.rehydrate(fresh_instance(&hibernated)), Ok(()));
        assert_eq!(hibernated.state(), LifecycleTier::Parked);
        assert!(
            hibernated.enter().is_ok(),
            "a restored instance can be called into"
        );
    }

    #[test]
    fn parking_is_refused_while_a_call_holds_the_instance() {
        let pool = pool();
        let slot = slot_in(&pool, LifecycleTier::Parked);
        let guard = slot.enter().expect("enter");
        assert_eq!(
            slot.park(),
            Err(TransitionError {
                instance: slot.id,
                from: LifecycleTier::Hot,
                to: LifecycleTier::Parked,
            }),
            "the call still owns the instance"
        );
        assert_eq!(
            slot.state(),
            LifecycleTier::Hot,
            "a refused park must not publish Parked under the call"
        );
        assert_eq!(slot.parks(), 0, "a refused park is not a boundary");
        assert_eq!(
            slot.enter().err(),
            Some(EnterError::Busy),
            "the instance is still the call's alone"
        );
        drop(guard);
        assert_eq!(
            slot.state(),
            LifecycleTier::Parked,
            "the returning call parks the instance"
        );
        assert_eq!(slot.park(), Ok(()), "and an empty stack can be recorded");
        assert_eq!(slot.parks(), 1);
    }

    #[test]
    fn a_refused_transition_names_the_instance_and_both_tiers() {
        let pool = pool();
        let discarded = slot_in(&pool, LifecycleTier::Discarded);
        assert_eq!(
            discarded.park(),
            Err(TransitionError {
                instance: discarded.id,
                from: LifecycleTier::Discarded,
                to: LifecycleTier::Parked,
            }),
            "a discarded instance cannot be parked"
        );
        assert_eq!(discarded.parks(), 0, "a refused park is not a boundary");
        assert_eq!(
            discarded.rehydrate(fresh_instance(&discarded)),
            Err(TransitionError {
                instance: discarded.id,
                from: LifecycleTier::Discarded,
                to: LifecycleTier::Parked,
            }),
            "a discarded instance cannot be restored"
        );
        assert!(
            matches!(
                discarded.enter().err(),
                Some(EnterError::NotResident(LifecycleTier::Discarded))
            ),
            "a refused restore must not leave an instance to call into"
        );

        let hibernated = slot_in(&pool, LifecycleTier::Hibernated);
        assert_eq!(
            hibernated.park(),
            Err(TransitionError {
                instance: hibernated.id,
                from: LifecycleTier::Hibernated,
                to: LifecycleTier::Parked,
            }),
            "parking cannot stand in for restoring a hibernated instance"
        );
        assert_eq!(
            hibernated.hibernate(),
            Err(TransitionError {
                instance: hibernated.id,
                from: LifecycleTier::Hibernated,
                to: LifecycleTier::Hibernated,
            }),
            "a hibernated instance has nothing left to capture"
        );
    }

    #[test]
    fn an_instance_discarded_during_a_call_stays_discarded_when_the_call_returns() {
        let pool = pool();
        let slot = slot_in(&pool, LifecycleTier::Parked);
        let guard = slot.enter().expect("enter");
        slot.discard();
        let refused = |to| {
            Err(TransitionError {
                instance: slot.id,
                from: LifecycleTier::Discarded,
                to,
            })
        };
        assert_eq!(
            slot.hibernate(),
            refused(LifecycleTier::Hibernated),
            "a hibernation refused under the call names the tier the instance is in"
        );
        assert_eq!(
            slot.park(),
            refused(LifecycleTier::Parked),
            "a park refused under the call names the tier the instance is in"
        );
        assert_eq!(
            slot.rehydrate(fresh_instance(&slot)),
            refused(LifecycleTier::Parked),
            "a restore refused under the call names the tier the instance is in"
        );
        drop(guard);
        assert_eq!(
            slot.state(),
            LifecycleTier::Discarded,
            "a returning call must not revive a discarded instance"
        );
        assert!(
            matches!(
                slot.enter().err(),
                Some(EnterError::NotResident(LifecycleTier::Discarded))
            ),
            "and it leaves nothing to call into"
        );
    }

    #[test]
    fn a_second_call_into_a_busy_instance_is_impossible() {
        let p = pool();
        let (slot, _) = p.acquire(ctx_for).expect("acquire");
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
            "the instance is available again once the slice returns"
        );
    }

    #[test]
    fn a_hibernated_slot_has_nothing_to_call_into() {
        let p = pool();
        let (slot, _) = p.acquire(ctx_for).expect("acquire");
        slot.hibernate().expect("hibernate");
        assert_eq!(
            slot.enter().err(),
            Some(EnterError::NotResident(LifecycleTier::Hibernated)),
            "a hibernated slot has no memory to call into"
        );
    }

    #[test]
    fn capture_is_refused_unless_parked() {
        let p = pool();
        let (slot, _) = p.acquire(ctx_for).expect("acquire");
        let guard = slot.enter().expect("enter");
        assert!(
            !slot.is_capture_eligible(),
            "a slot with a call in flight is not capturable"
        );
        assert_eq!(
            slot.hibernate(),
            Err(TransitionError {
                instance: slot.id,
                from: LifecycleTier::Hot,
                to: LifecycleTier::Hibernated,
            }),
            "hibernating a Hot slot is refused"
        );
        drop(guard);
        assert!(slot.is_capture_eligible(), "a parked slot is capturable");
        assert!(
            slot.hibernate().is_ok(),
            "hibernating a parked slot succeeds"
        );
    }
}
