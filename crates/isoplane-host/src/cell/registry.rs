//! Module registration and the resident-instance bookkeeping tables.

use super::*;

pub(super) struct ModuleEntry {
    pub(super) tenant: String,
    pub(super) limits: Limits,
    pub(super) debug_cpu_ms: Option<u32>,
    pub(super) manifest: AppManifest,
    pub(super) activator: Arc<dyn Activator>,
    pub(super) pool: InstancePool,
    pub(super) meters: Arc<Meters>,
    pub(super) endpoint_exceptions: Arc<[std::net::SocketAddr]>,
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
    pub(super) completion: Arc<DispatchCompletion>,
}

impl Cell {
    /// Sets activation configuration for a registered module owned by `tenant`.
    /// The blob remains host-resident and is unavailable during preparation.
    pub fn set_runtime_configuration(
        &self,
        tenant: &str,
        module: &ModuleId,
        configuration: crate::bindings::RuntimeConfiguration,
    ) -> Result<(), crate::bindings::RuntimeConfigurationError> {
        use crate::bindings::RuntimeConfigurationError;
        let modules = self.modules.read();
        let entry = modules
            .get(module)
            .ok_or(RuntimeConfigurationError::UnknownModule)?;
        if entry.tenant != tenant {
            return Err(RuntimeConfigurationError::WrongTenant);
        }
        self.rt
            .runtime_configuration
            .set(tenant, module, configuration);
        Ok(())
    }

    pub(crate) fn module_imports(&self, module: &ModuleId, name: &str) -> bool {
        self.modules.read().get(module).is_some_and(|entry| {
            entry
                .activator
                .imports(isoplane_abi::imports::HOST_MODULE, name)
        })
    }

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
        self.register_execution(tenant, activator, manifest, None, Arc::new([]))
    }

    /// Registers a module whose instances may also reach `exceptions`, exact
    /// peers the node's destination policy refuses.
    pub fn register_module_with_exceptions(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<dyn Activator>,
        manifest: AppManifest,
        exceptions: Arc<[std::net::SocketAddr]>,
    ) -> crate::Result<()> {
        self.register_execution(tenant, activator, manifest, None, exceptions)
    }

    /// Registers a fresh local debug guest with no idle pooling or capture.
    /// The caller must use the local debug wall backstop for its cell.
    pub fn register_debug_module(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<crate::debug::DebugActivator>,
        mut manifest: AppManifest,
    ) -> crate::Result<()> {
        anyhow::ensure!(
            self.cfg.wall_ms_cap == crate::debug::DEBUG_BUDGET_MS,
            "local debugging requires its explicit cell wall backstop"
        );
        manifest.handlers.lifecycle = HandlerLifecycle::Fresh;
        self.register_execution(
            tenant,
            activator,
            manifest,
            Some(crate::debug::DEBUG_BUDGET_MS),
            Arc::new([]),
        )
    }

    fn register_execution(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<dyn Activator>,
        manifest: AppManifest,
        debug_cpu_ms: Option<u32>,
        endpoint_exceptions: Arc<[std::net::SocketAddr]>,
    ) -> crate::Result<()> {
        anyhow::ensure!(
            !tenant.trim().is_empty(),
            "module registration requires tenant identity"
        );
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
            debug_cpu_ms,
            manifest,
            pool: InstancePool::new(
                activator.clone(),
                lifecycle,
                if debug_cpu_ms.is_some() {
                    0
                } else {
                    self.cfg.max_idle_per_module
                },
            ),
            activator,
            meters: Arc::new(Meters::new()),
            endpoint_exceptions,
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
        let mut context = InstanceCtx::new(
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
        );
        context.limits = entry.manifest.effective_limits();
        context.endpoint_exceptions = entry.endpoint_exceptions.clone();
        context.configure_resources(self.cfg.guest_resources);
        Arc::new(context)
    }

    pub(super) fn acquire_handler(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<(Arc<Slot>, AcquireSource)> {
        let (slot, src) = entry.pool.acquire_reserved(
            |id| {
                let ctx = self.make_ctx(entry, id);
                let maximum = entry
                    .activator
                    .snapshot_size_bound(ctx.limits.instance_memory_max)?;
                let reservation = tracing::info_span!(
                    "snapshot_reservation",
                    maximum_bytes = maximum
                )
                .in_scope(|| {
                    self.reserve_snapshot(isoplane_hibernate::snapshot::SnapshotBytes(maximum))
                })?;
                *ctx.snapshot.lock() = Some(reservation);
                Ok(ctx)
            },
            |slot| self.forget(slot),
            |slot| self.wake(slot, entry),
        )?;
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
