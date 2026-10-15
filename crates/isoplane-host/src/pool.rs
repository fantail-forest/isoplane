//! The activator contract and the per-module pool of Handler instances that draws from it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_abi::config::HandlerLifecycle;
use isoplane_engine::{InstanceOps, ModuleHash};
use parking_lot::Mutex;

use crate::bindings::InstanceCtx;
use crate::disposition::{policy_reset, Disposition, DispositionKind};
use crate::slot::Slot;
use crate::{InstanceId, ModuleId};

/// What the cell needs from a compiled, pre-initialized module. A produced
/// instance is `Send` because a parked instance is woken onto whichever pool
/// thread takes the wake.
pub trait Activator: Send + Sync + 'static {
    /// Pre-map one captured instance at registration when supported.
    fn map_activation(
        &self,
        _memory_limit_bytes: isoplane_abi::config::GuestMemoryBytes,
    ) -> anyhow::Result<Option<Box<dyn isoplane_engine::DormantInstance>>> {
        Ok(None)
    }
    fn module(&self) -> &ModuleId;
    fn module_hash(&self) -> ModuleHash;
    fn abi_rev(&self) -> u32;
    /// Maximum encoded local snapshot bytes under the effective memory limit.
    fn snapshot_size_bound(&self, max_memory_bytes: u64) -> anyhow::Result<u64>;
    /// Whether the compiled guest declares a capability; scripted activators default to legacy delivery.
    fn imports(&self, _module: &str, _name: &str) -> bool {
        false
    }

    /// The baseline hibernation diffs against.
    fn template_image(&self) -> Arc<isoplane_engine::MemoryImage>;
    fn activate(
        &self,
        bindings: Box<dyn isoplane_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>>;
}

/// What a dispatch paid for its instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireSource {
    /// A parked handler eligible for reuse.
    WarmParked,
    /// A reset Handler instance from the pool.
    PooledFresh,
    /// An instance woken from its hibernation delta with retained state.
    HibernationWake,
    /// A cold activation from the template.
    Activated,
}

impl AcquireSource {
    pub fn label(self) -> &'static str {
        match self {
            AcquireSource::WarmParked => "warm",
            AcquireSource::PooledFresh => "pooled_fresh",
            AcquireSource::HibernationWake => "hibernation_wake",
            AcquireSource::Activated => "activated",
        }
    }
}

/// A per-module pool of Handler instances.
pub struct InstancePool {
    pub module: ModuleId,
    pub lifecycle: HandlerLifecycle,
    activator: Arc<dyn Activator>,
    warm: Mutex<IdleList>,
    reset_candidates: Mutex<IdleList>,
    max_idle: usize,
    mapped: Mutex<Option<Box<dyn isoplane_engine::DormantInstance>>>,
    live: AtomicU64,
    /// Set once the module is retired; instances released afterwards are
    /// discarded instead of pooled.
    retired: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct IdleList {
    slots: Vec<Arc<Slot>>,
    claimed: usize,
    generation: Arc<()>,
}

/// Exclusive ownership of one idle pool entry during reclamation.
/// Returning the claim preserves its list and lifecycle state; a discarded
/// entry, or one claimed before a pool drain, leaves live accounting instead.
pub(crate) struct IdleClaim<'a> {
    list: &'a Mutex<IdleList>,
    live: &'a AtomicU64,
    slot: Arc<Slot>,
    generation: Arc<()>,
}

impl IdleClaim<'_> {
    /// The entry reserved from acquisition until this claim is dropped.
    pub(crate) fn slot(&self) -> &Arc<Slot> {
        &self.slot
    }
}

impl Drop for IdleClaim<'_> {
    fn drop(&mut self) {
        let mut list = self.list.lock();
        list.claimed -= 1;
        if self.slot.state() == crate::LifecycleTier::Discarded
            || !Arc::ptr_eq(&self.generation, &list.generation)
        {
            drop(list);
            self.slot.discard();
            self.live.fetch_sub(1, Ordering::Relaxed);
        } else {
            list.slots.push(self.slot.clone());
        }
    }
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
            warm: Mutex::new(IdleList::default()),
            reset_candidates: Mutex::new(IdleList::default()),
            max_idle,
            mapped: Mutex::new(None),
            live: AtomicU64::new(0),
            retired: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn map_activation(
        &self,
        memory_limit_bytes: isoplane_abi::config::GuestMemoryBytes,
    ) -> anyhow::Result<()> {
        *self.mapped.lock() = self.activator.map_activation(memory_limit_bytes)?;
        Ok(())
    }

    pub(crate) fn activate(
        &self,
        bindings: Box<dyn isoplane_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let mapped = self.mapped.lock().take();
        match mapped {
            Some(mapped) => mapped.activate(bindings),
            None => self.activator.activate(bindings),
        }
    }

    /// Discards the idle instances and every instance released from now on.
    /// Returns how many idle instances were discarded.
    pub fn retire(&self) -> usize {
        // Set before draining takes each list lock, and read under that lock by
        // `release`, so an instance released concurrently is drained or refused.
        self.retired.store(true, Ordering::Release);
        self.mapped.lock().take();
        self.drain_idle()
    }

    pub fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }

    /// Idle instance counts as `(warm, awaiting reset)`.
    pub fn idle_counts(&self) -> (usize, usize) {
        (
            self.warm.lock().slots.len(),
            self.reset_candidates.lock().slots.len(),
        )
    }

    /// Removes an idle entry from acquisition until its claim is returned.
    pub(crate) fn claim_idle(&self, id: InstanceId) -> Option<IdleClaim<'_>> {
        for list in [&self.warm, &self.reset_candidates] {
            let mut entries = list.lock();
            if let Some(index) = entries.slots.iter().position(|slot| slot.id == id) {
                let slot = entries.slots.swap_remove(index);
                entries.claimed += 1;
                return Some(IdleClaim {
                    list,
                    live: &self.live,
                    slot,
                    generation: entries.generation.clone(),
                });
            }
        }
        None
    }

    /// Takes this warm instance for a dispatch, if it is still idle and
    /// `eligible`; [`InstancePool::release`] returns it.
    pub(crate) fn take_idle(
        &self,
        id: InstanceId,
        eligible: impl Fn(&Slot) -> bool,
    ) -> Option<Arc<Slot>> {
        let mut warm = self.warm.lock();
        let index = warm
            .slots
            .iter()
            .position(|slot| slot.id == id && eligible(slot))?;
        Some(warm.slots.swap_remove(index))
    }

    /// Reuse a warm instance, reset a candidate for this acquisition, or activate.
    /// Fresh handlers never consult the warm list.
    pub fn acquire<F>(&self, make_ctx: F) -> anyhow::Result<(Arc<Slot>, AcquireSource)>
    where
        F: FnOnce(InstanceId) -> Arc<InstanceCtx>,
    {
        self.acquire_with_cleanup(make_ctx, |_| {})
    }

    /// Calls `discard` synchronously for failed candidates so their owner can
    /// remove registry entries before replacement activation, including when it fails.
    pub(crate) fn acquire_with_cleanup<F, D>(
        &self,
        make_ctx: F,
        mut discard: D,
    ) -> anyhow::Result<(Arc<Slot>, AcquireSource)>
    where
        F: FnOnce(InstanceId) -> Arc<InstanceCtx>,
        D: FnMut(&Arc<Slot>),
    {
        self.acquire_reserved(
            |id| Ok(make_ctx(id)),
            &mut discard,
            |_| anyhow::bail!("this pool acquisition has no snapshot restoration provider"),
        )
    }

    pub(crate) fn acquire_reserved<F, D, R>(
        &self,
        make_ctx: F,
        mut discard: D,
        mut restore: R,
    ) -> anyhow::Result<(Arc<Slot>, AcquireSource)>
    where
        F: FnOnce(InstanceId) -> anyhow::Result<Arc<InstanceCtx>>,
        D: FnMut(&Arc<Slot>),
        R: FnMut(&Arc<Slot>) -> anyhow::Result<()>,
    {
        if self.lifecycle == HandlerLifecycle::Reuse {
            loop {
                let candidate = self.warm.lock().slots.pop();
                let Some(slot) = candidate else { break };
                let source = {
                    let _execution = slot.lock_execution();
                    match slot.state() {
                        crate::LifecycleTier::Discarded => {
                            self.live.fetch_sub(1, Ordering::Relaxed);
                            discard(&slot);
                            continue;
                        }
                        crate::LifecycleTier::Hibernated => {
                            if let Err(error) = restore(&slot) {
                                slot.discard();
                                self.live.fetch_sub(1, Ordering::Relaxed);
                                discard(&slot);
                                return Err(error);
                            }
                            AcquireSource::HibernationWake
                        }
                        crate::LifecycleTier::Parked => AcquireSource::WarmParked,
                        crate::LifecycleTier::Hot => AcquireSource::WarmParked,
                    }
                };
                return Ok((slot, source));
            }
        }
        loop {
            let candidate = self.reset_candidates.lock().slots.pop();
            let Some(slot) = candidate else { break };
            {
                let _execution = slot.lock_execution();
                if slot.state() == crate::LifecycleTier::Hibernated {
                    if let Err(error) = restore(&slot) {
                        slot.discard();
                        self.live.fetch_sub(1, Ordering::Relaxed);
                        discard(&slot);
                        return Err(error);
                    }
                }
                if let Err(error) = slot.reset() {
                    tracing::warn!(module = %self.module, instance = %slot.id, %error,
                        "pooled instance reset failed; discarding candidate");
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                    discard(&slot);
                    continue;
                }
            }
            return Ok((slot, AcquireSource::PooledFresh));
        }
        let id = InstanceId::alloc();
        let ctx = make_ctx(id)?;
        let bindings = crate::bindings::CellBindings::boxed(ctx.clone());
        let inst = self.activate(bindings)?;
        self.live.fetch_add(1, Ordering::Relaxed);
        Ok((Arc::new(Slot::new(id, ctx, inst)), AcquireSource::Activated))
    }

    /// The only way back into the pool.
    pub fn release(&self, slot: Arc<Slot>, d: Disposition) {
        match d.kind() {
            DispositionKind::Warm => {
                if self.lifecycle == HandlerLifecycle::Fresh {
                    // Fails closed to the hygienic side: residue never leaks
                    // from a fresh app whatever produced the disposition.
                    return self.release(slot, policy_reset());
                }
                slot.park(crate::now_ms());
                let mut w = self.warm.lock();
                if !self.retired.load(Ordering::Acquire)
                    && w.slots.len() + w.claimed < self.max_idle
                {
                    w.slots.push(slot);
                } else {
                    drop(w);
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                }
            }
            DispositionKind::Reset => {
                slot.ctx.release_native_resources();
                slot.park(crate::now_ms());
                let mut candidates = self.reset_candidates.lock();
                if !self.retired.load(Ordering::Acquire)
                    && candidates.slots.len() + candidates.claimed < self.max_idle
                {
                    candidates.slots.push(slot);
                } else {
                    drop(candidates);
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

    /// Discards available idle entries and invalidates outstanding idle claims.
    /// Returns the number discarded synchronously; claims discard on return.
    pub fn drain_idle(&self) -> usize {
        let mut n = 0;
        for list in [&self.warm, &self.reset_candidates] {
            let slots = {
                let mut list = list.lock();
                list.generation = Arc::new(());
                std::mem::take(&mut list.slots)
            };
            for slot in slots {
                slot.discard();
                self.live.fetch_sub(1, Ordering::Relaxed);
                n += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod reset_tests;

#[cfg(test)]
mod claim_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposition::{decide, RequestEnd};
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use isoplane_abi::config::Limits;
    use isoplane_engine::Completion;

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

    #[test]
    fn an_instance_released_after_retirement_is_discarded_not_pooled() {
        for lifecycle in [HandlerLifecycle::Reuse, HandlerLifecycle::Fresh] {
            let p = pool(lifecycle);
            let (slot, _) = p.acquire(|id| ctx_for(id, lifecycle)).expect("acquire");
            p.retire();
            let RequestEnd::Disposed(disposition) = decide(lifecycle, Completion::Done, false)
            else {
                panic!("a completed request must dispose of its instance");
            };
            p.release(slot.clone(), disposition);
            assert_eq!(
                p.idle_counts(),
                (0, 0),
                "{lifecycle:?}: a retired module must not pool a released instance"
            );
            assert_eq!(
                slot.state(),
                crate::slot::LifecycleTier::Discarded,
                "{lifecycle:?}: the released instance must be discarded"
            );
        }
    }

    #[test]
    fn fresh_never_touches_the_warm_pool() {
        let p = pool(HandlerLifecycle::Fresh);
        let (a, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("acquire");
        assert_eq!(src, AcquireSource::Activated, "an empty pool activates");
        let id_a = a.id;
        let RequestEnd::Disposed(d) = decide(HandlerLifecycle::Fresh, Completion::Done, false)
        else {
            panic!("expected a terminal disposition");
        };
        p.release(a, d);
        let (warm, fresh) = p.idle_counts();
        assert_eq!(warm, 0, "a fresh app must never populate the warm pool");
        assert_eq!(fresh, 1, "the pending reset candidate is pooled");

        let (b, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("acquire");
        assert_eq!(
            src,
            AcquireSource::PooledFresh,
            "the next acquisition resets the pooled candidate"
        );
        assert_eq!(b.id, id_a, "the same slot comes back");
        assert!(b.is_pristine(), "and it carries no residue");
    }

    #[test]
    fn residue_in_a_reset_candidate_is_cleared_before_acquisition() {
        let p = pool(HandlerLifecycle::Fresh);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("activate");
        slot.enter()
            .expect("write request residue")
            .instance()
            .memory()[0] = 99;
        p.release(slot.clone(), policy_reset());
        assert_eq!(
            slot.enter()
                .expect("inspect pending reset")
                .instance()
                .memory()[0],
            99,
            "release must not reset guest memory without a new request"
        );
        let (reused, source) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("reset candidate");
        assert_eq!(
            source,
            AcquireSource::PooledFresh,
            "acquisition resets the candidate"
        );
        assert_eq!(
            reused.id, slot.id,
            "reset can preserve the existing slot identity"
        );
        assert!(
            reused.is_pristine(),
            "fresh acquisition must not carry request residue"
        );
        assert_eq!(
            reused
                .enter()
                .expect("inspect reset memory")
                .instance()
                .memory()[0],
            0,
            "reset must restore template memory before handing the slot to a request"
        );
    }

    #[test]
    fn reuse_prefers_warm_then_fresh_then_activate() {
        let p = pool(HandlerLifecycle::Reuse);
        let (a, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        assert_eq!(src, AcquireSource::Activated, "an empty pool activates");
        let (b, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        let warm_id = a.id;
        let fresh_id = b.id;
        let RequestEnd::Disposed(warm) = decide(HandlerLifecycle::Reuse, Completion::Done, false)
        else {
            panic!("a completed reuse dispatch stays warm");
        };
        p.release(a, warm);
        p.release(b, policy_reset());

        let (got, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        assert_eq!(src, AcquireSource::WarmParked, "warm first");
        assert_eq!(got.id, warm_id, "and it is the warm instance");

        let (got, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        assert_eq!(src, AcquireSource::PooledFresh, "then pooled fresh");
        assert_eq!(got.id, fresh_id, "and it is the reset instance");

        let (_, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        assert_eq!(src, AcquireSource::Activated, "then a cold activation");
    }

    #[test]
    fn a_killed_instance_leaves_the_pool_entirely() {
        let p = pool(HandlerLifecycle::Reuse);
        let (a, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Reuse))
            .expect("acquire");
        let RequestEnd::Disposed(d) =
            decide(HandlerLifecycle::Reuse, Completion::BudgetKill, false)
        else {
            panic!("a kill is terminal")
        };
        p.release(a, d);
        assert_eq!(p.idle_counts(), (0, 0), "a killed instance is not pooled");
        assert_eq!(p.live(), 0, "and it is no longer live");
    }
}
