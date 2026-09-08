//! The activator contract and the per-module pool of Handler instances that draws from it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::config::HandlerLifecycle;
use wisp_engine::{InstanceOps, ModuleHash};

use crate::bindings::InstanceCtx;
use crate::disposition::{policy_reset, Disposition, DispositionKind};
use crate::slot::Slot;
use crate::{InstanceId, ModuleId};

/// What the cell needs from a compiled, pre-initialized module. A produced
/// instance is `Send` because a parked instance is woken onto whichever pool
/// thread takes the wake.
pub trait Activator: Send + Sync + 'static {
    fn module(&self) -> &ModuleId;
    fn module_hash(&self) -> ModuleHash;
    fn abi_rev(&self) -> u32;
    /// The baseline hibernation diffs against.
    fn template_image(&self) -> Arc<wisp_engine::MemoryImage>;
    fn activate(
        &self,
        bindings: Box<dyn wisp_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>>;
}

/// What a dispatch paid for its instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireSource {
    /// A parked instance with residue: `reuse` Handlers, and a resident
    /// durable instance.
    WarmParked,
    /// A reset Handler instance from the pool.
    PooledFresh,
    /// A durable instance woken from its hibernation delta.
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

#[derive(Debug, thiserror::Error)]
enum PoolError {
    #[error("module {module} pooled instance {instance} carried residue after reset")]
    Residue {
        module: ModuleId,
        instance: InstanceId,
    },
}

/// A per-module pool of Handler instances.
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

    pub fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }

    pub fn idle_counts(&self) -> (usize, usize) {
        (self.warm.lock().len(), self.fresh.lock().len())
    }

    /// Warm-parked, then pooled-fresh, then activate; under `fresh` the warm
    /// list is not consulted.
    pub fn acquire<F>(&self, make_ctx: F) -> anyhow::Result<(Arc<Slot>, AcquireSource)>
    where
        F: FnOnce(InstanceId) -> Arc<InstanceCtx>,
    {
        let pooled = (self.lifecycle == HandlerLifecycle::Reuse)
            .then(|| self.warm.lock().pop())
            .flatten()
            .map(|s| (s, AcquireSource::WarmParked))
            .or_else(|| {
                self.fresh
                    .lock()
                    .pop()
                    .map(|s| (s, AcquireSource::PooledFresh))
            });
        if let Some((slot, source)) = pooled {
            if source == AcquireSource::PooledFresh {
                if !slot.is_pristine() {
                    slot.tombstone();
                    slot.discard();
                    self.live.fetch_sub(1, Ordering::Relaxed);
                    return Err(PoolError::Residue {
                        module: self.module.clone(),
                        instance: slot.id,
                    }
                    .into());
                }
                debug_assert!(
                    slot.is_pristine(),
                    "a pooled-fresh instance carried residue"
                );
            }
            return Ok((slot, source));
        }
        let id = InstanceId::alloc();
        let ctx = make_ctx(id);
        let bindings = crate::bindings::CellBindings::boxed(ctx.clone());
        let inst = self.activator.activate(bindings)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposition::{decide, RequestEnd};
    use crate::fakes::{FakeActivator, FakeBehaviour};
    use wisp_abi::config::Limits;
    use wisp_engine::Completion;

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
        assert_eq!(fresh, 1, "the reset instance is pooled");

        let (b, src) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("acquire");
        assert_eq!(
            src,
            AcquireSource::PooledFresh,
            "the next acquisition draws the reset instance"
        );
        assert_eq!(b.id, id_a, "the same slot comes back");
        assert!(b.is_pristine(), "and it carries no residue");
    }

    #[test]
    fn residue_in_the_fresh_pool_is_refused_and_discarded() {
        let p = pool(HandlerLifecycle::Fresh);
        let (slot, _) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("activate");
        p.release(slot.clone(), policy_reset());
        drop(slot.enter().expect("inject residue after reset"));
        let error = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .err()
            .expect("residue is refused");
        assert!(
            matches!(error.downcast_ref::<PoolError>(), Some(PoolError::Residue { instance, .. }) if *instance == slot.id),
            "acquisition identifies the contaminated instance"
        );
        assert!(
            slot.is_tombstoned(),
            "contaminated pooled memory cannot be entered again"
        );
        assert_eq!(
            p.live(),
            0,
            "the refused instance is removed from pool accounting"
        );
        assert_eq!(
            p.idle_counts(),
            (0, 0),
            "the contaminated instance is not pooled"
        );
        let (replacement, source) = p
            .acquire(|id| ctx_for(id, HandlerLifecycle::Fresh))
            .expect("replacement activates");
        assert_eq!(
            source,
            AcquireSource::Activated,
            "a later dispatch can activate normally"
        );
        assert_ne!(
            replacement.id, slot.id,
            "contaminated memory is never reused"
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
