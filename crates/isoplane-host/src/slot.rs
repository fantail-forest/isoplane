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
    /// hard-killed or trapped instance is discarded, because its memory may be
    /// mid-collection or mid-frame and wasmtime would still answer a call on
    /// the killed store.
    NotResident(LifecycleTier),
}

impl std::fmt::Display for EnterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnterError::Busy => write!(f, "reentrant call refused"),
            EnterError::NotResident(s) => write!(f, "instance is {}", s.as_str()),
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
    dormant: Option<Box<dyn isoplane_engine::DormantInstance>>,
    in_call: bool,
    parked_since_ms: Option<u64>,
    /// Set on entry or rehydration and cleared by reset.
    dirty: bool,
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
    last_used_ms: AtomicU64,
    parks: AtomicU64,
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
                dormant: None,
                in_call: false,
                parked_since_ms: None,
                dirty: false,
            }),
            execution: Mutex::new(()),
            parked_ms_total: AtomicU64::new(0),
            state_hint: AtomicU8::new(tier_as_u8(LifecycleTier::Parked)),
            parked_since_hint: AtomicU64::new(0),
            last_used_ms: AtomicU64::new(crate::now_ms()),
            parks: AtomicU64::new(0),
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

    /// Most recent activity boundary, retained while the instance is hibernated.
    pub fn last_used_ms(&self) -> u64 {
        self.last_used_ms.load(Ordering::Relaxed)
    }

    /// How many times the instance has parked. Unlike [`Self::last_used_ms`]
    /// it differs after every activity boundary, whatever the clock read.
    pub fn parks(&self) -> u64 {
        self.parks.load(Ordering::Relaxed)
    }

    /// Capture is eligible only when Parked.
    pub fn is_capture_eligible(&self) -> bool {
        let g = self.inner.lock();
        !g.in_call && g.state == LifecycleTier::Parked && g.resident.is_some()
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

    pub fn park(&self, now_ms: u64) {
        self.last_used_ms.store(now_ms, Ordering::Relaxed);
        self.parks.fetch_add(1, Ordering::Relaxed);
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

    /// Releases guest pages after saving a local snapshot, retaining mappings when supported.
    pub fn hibernate(&self) -> Result<(), HibernateRefused> {
        let mut g = self.inner.lock();
        if g.in_call {
            return Err(HibernateRefused::Hot);
        }
        if g.state != LifecycleTier::Parked {
            return Err(HibernateRefused::NotParked(g.state));
        }
        g.dormant = g.resident.take().and_then(|instance| instance.evict());
        g.state = LifecycleTier::Hibernated;
        g.parked_since_ms = None;
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

    /// Install the caller's restored instance and return the slot to parked.
    pub fn rehydrate(&self, inst: Box<dyn InstanceOps + Send>) {
        let mut g = self.inner.lock();
        debug_assert_eq!(g.state, LifecycleTier::Hibernated);
        g.resident = Some(inst);
        g.state = LifecycleTier::Parked;
        g.dormant = None;
        g.dirty = true;
        self.publish(&g);
    }

    /// Reset to template contents. Refused for a discarded instance, because
    /// an `Ok` here is what pool acquisition takes as "reusable".
    pub fn reset(&self) -> anyhow::Result<()> {
        let mut g = self.inner.lock();
        if g.state == LifecycleTier::Discarded {
            anyhow::bail!(
                "refusing to reset a discarded slot: a killed instance is not laundered back \
                 into the pool"
            );
        }
        if g.in_call {
            anyhow::bail!("cannot reset an instance with a call in flight");
        }
        self.ctx.release_native_resources();
        if let Some(i) = g.resident.as_mut() {
            if let Err(error) = i.reset() {
                g.resident = None;
                g.state = LifecycleTier::Discarded;
                self.ctx.release_native_resources();
                self.publish(&g);
                return Err(error);
            }
        }
        g.dirty = false;
        g.parked_since_ms = None;
        g.state = LifecycleTier::Parked;
        self.publish(&g);
        Ok(())
    }

    pub fn discard(&self) {
        let mut g = self.inner.lock();
        g.resident = None;
        g.dormant = None;
        self.ctx.snapshot.lock().take();
        self.ctx.release_native_resources();
        g.state = LifecycleTier::Discarded;
        g.parked_since_ms = None;
        self.publish(&g);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HibernateRefused {
    /// A call is in flight, so the stack is not empty.
    Hot,
    NotParked(LifecycleTier),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use crate::pool::InstancePool;
    use isoplane_abi::config::Limits;

    fn ctx_for(id: InstanceId) -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::for_test(id, Limits::default()))
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
            timers_between_requests_ms: None,
            network: Some(isoplane_abi::native_io::ResourceHandle(3)),
            next_handle: 5,
        };
        let bytes = postcard::to_allocvec(&prepared)
            .unwrap()
            .into_iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>();
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

    fn assert_prepared_native_baseline(slot: &Slot) {
        use isoplane_engine::HostBindings;
        let outcome = slot.enter().unwrap().instance().gc_hint(0);
        assert_eq!(
            outcome.completion,
            isoplane_engine::Completion::Done,
            "the fresh guest must resolve its captured network handle on its first entry"
        );
        let mut bindings = crate::bindings::CellBindings::new(slot.ctx.clone());
        assert!(
            matches!(
                bindings.native_io(isoplane_abi::native_io::Request::Network),
                Ok(isoplane_abi::native_io::Response::Handle(
                    isoplane_abi::native_io::ResourceHandle(3)
                ))
            ),
            "fresh guest memory must retain its prepared network authority"
        );
        assert!(
            matches!(
                bindings.native_io(isoplane_abi::native_io::Request::Ready(
                    isoplane_abi::native_io::ResourceHandle(4)
                )),
                Ok(isoplane_abi::native_io::Response::Ready(true))
            ),
            "a reset must restore the guest-cached inert pollable"
        );
        assert_eq!(
            pollable(slot).0,
            5,
            "a reset must restart allocation at the captured next handle"
        );
    }

    #[test]
    fn slot_reset_releases_old_resources_before_restoring_the_prepared_baseline() {
        let (_engine, _template, ctx, instance) = prepared_native_instance();
        let slot = Slot::new(ctx.instance, ctx, Box::new(instance));
        assert_prepared_native_baseline(&slot);
        let retired = pollable(&slot);
        slot.reset()
            .expect("a reset must restore native authority before the first guest entry");
        assert_pollable_released(&slot, retired);
        assert_prepared_native_baseline(&slot);
    }

    #[test]
    fn direct_engine_reset_restores_native_resources_without_slot_cleanup() {
        let (_engine, _template, ctx, mut instance) = prepared_native_instance();
        use isoplane_engine::HostBindings;
        let mut bindings = crate::bindings::CellBindings::new(ctx.clone());
        bindings
            .native_io(isoplane_abi::native_io::Request::Timer(
                isoplane_abi::native_io::DurationNanos(0),
            ))
            .unwrap();
        bindings
            .native_io(isoplane_abi::native_io::Request::Drop {
                handle: isoplane_abi::native_io::ResourceHandle(4),
                kind: isoplane_abi::native_io::ResourceKind::Pollable,
            })
            .unwrap();
        instance.reset().expect(
            "the engine reset must release host resources before restoring template authority",
        );
        let slot = Slot::new(ctx.instance, ctx, Box::new(instance));
        assert_prepared_native_baseline(&slot);
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
    fn resetting_to_pristine_memory_releases_native_resources() {
        let pool = pool();
        let (slot, _) = pool.acquire(ctx_for).expect("the slot must activate");
        let handle = pollable(&slot);
        slot.reset()
            .expect("the idle slot must reset to its template");
        assert_pollable_released(&slot, handle);
    }

    #[test]
    fn discarding_a_slot_releases_native_resources_even_while_its_context_is_retained() {
        let pool = pool();
        let (slot, _) = pool.acquire(ctx_for).expect("the slot must activate");
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
        slot.park(0);
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
            Err(HibernateRefused::Hot),
            "hibernating a Hot slot is refused"
        );
        drop(guard);
        slot.park(0);
        assert!(slot.is_capture_eligible(), "a parked slot is capturable");
        assert!(
            slot.hibernate().is_ok(),
            "hibernating a parked slot succeeds"
        );
    }
}
