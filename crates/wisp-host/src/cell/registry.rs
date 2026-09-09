//! Module registration and the resident-instance bookkeeping tables.

use super::*;

pub(super) struct ModuleEntry {
    pub(super) tenant: String,
    pub(super) limits: Limits,
    pub(super) manifest: AppManifest,
    pub(super) activator: Arc<dyn Activator>,
    pub(super) pool: InstancePool,
    pub(super) meters: Arc<Meters>,
}

/// The cell's bookkeeping for one resident instance.
pub(super) struct Resident {
    pub(super) slot: Arc<Slot>,
    pub(super) module: ModuleId,
}

impl Resident {}

pub(super) struct ActiveRequest {
    pub(super) dispatch_id: u64,
    pub(super) lifecycle: HandlerLifecycle,
    pub(super) module: ModuleId,
    pub(super) route: String,
    pub(super) responder: Option<std::sync::mpsc::SyncSender<DispatchResult>>,
    /// `None` when the clock could not be read at admission.
    pub(super) started_ns: Option<u64>,
    pub(super) cpu_ns: u64,
    pub(super) wall_ns: u64,
    pub(super) slices: u32,
    pub(super) acquired: &'static str,
    pub(super) budget: CpuBudget,
}

impl Cell {
    /// Whether this owner is permitted by the cell's quarantine state.
    pub fn admits(&self, tenant: &str) -> bool {
        !self.quarantined.lock().iter().any(|t| t == tenant)
    }

    pub fn register_module(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<dyn Activator>,
        manifest: AppManifest,
    ) -> crate::Result<()> {
        let limits = manifest.limits;
        if !self.admits(tenant) {
            anyhow::bail!(
                "cell `{}` refuses quarantined tenant `{tenant}`",
                self.cfg.name
            );
        }
        let module = activator.module().clone();
        let lifecycle = manifest.handlers.lifecycle;
        let entry = ModuleEntry {
            tenant: tenant.to_string(),
            limits,
            manifest,
            pool: InstancePool::new(activator.clone(), lifecycle, self.cfg.max_idle_per_module),
            activator,
            meters: Arc::new(Meters::new()),
        };
        self.modules.write().insert(module, Arc::new(entry));
        Ok(())
    }

    pub fn meters(&self, module: &ModuleId) -> Option<Arc<Meters>> {
        self.modules.read().get(module).map(|m| m.meters.clone())
    }

    pub(super) fn forget(&self, slot: &Arc<Slot>) {
        self.index.lock().remove(&slot.id);
        self.residents.lock().remove(&slot.id);
    }

    pub(super) fn make_ctx(&self, entry: &Arc<ModuleEntry>, id: InstanceId) -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::new(
            id,
            Tenancy {
                module: entry.activator.module().clone(),
                tenant: entry.tenant.clone(),
                limits: entry.limits,
                lifecycle: entry.manifest.handlers.lifecycle,
                allowlist: entry.manifest.bindings.fetch_allowlist.clone(),
            },
            entry.meters.clone(),
            Some(self.rt.clone()),
        ))
    }

    pub(super) fn acquire_handler(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<(Arc<Slot>, AcquireSource)> {
        let (slot, src) = entry
            .pool
            .acquire_with_cleanup(|id| self.make_ctx(entry, id), |slot| self.forget(slot))?;
        self.index.lock().insert(slot.id, Arc::downgrade(&slot));
        let mut residents = self.residents.lock();
        residents.entry(slot.id).or_insert_with(|| Resident {
            slot: slot.clone(),
            module: entry.activator.module().clone(),
        });
        drop(residents);
        Ok((slot, src))
    }

    /// Falls back to the tombstone ring, so a killed instance refuses rather
    /// than reading as "no such instance".
    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        if let Some(s) = self.residents.lock().get(&id).map(|r| r.slot.clone()) {
            return Some(s);
        }
        self.tombstoned.lock().iter().find(|s| s.id == id).cloned()
    }

    pub fn module_tenant(&self, module: &ModuleId) -> Option<String> {
        self.modules.read().get(module).map(|m| m.tenant.clone())
    }

    pub fn instance_state(&self, id: InstanceId) -> Option<LifecycleTier> {
        self.residents.lock().get(&id).map(|r| r.slot.state())
    }
    pub fn resident_ids(&self) -> Vec<InstanceId> {
        self.residents.lock().keys().copied().collect()
    }
    /// Bytes buffered in a response handle. Chunks delivered through an HTTP
    /// transport are consumed by its requester and are not retained here.
    pub fn response_snapshot(&self, id: InstanceId, handle: u32) -> Option<Vec<u8>> {
        let g = self.residents.lock();
        let r = g.get(&id)?;
        let t = r.slot.ctx.responses.lock();
        t.get(handle).map(|resp| resp.body())
    }
}
