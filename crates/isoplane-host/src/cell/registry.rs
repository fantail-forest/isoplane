//! Module registration and the resident-instance bookkeeping tables.

use super::*;

pub(super) struct ModuleEntry {
    /// Instances that have had dispatches in progress, which a new dispatch
    /// may join; pruned as they are found to have none.
    pub(super) shared: Mutex<Vec<Weak<Slot>>>,
    pub(super) tenant: String,
    pub(super) limits: Limits,
    pub(super) debug_occupancy_ms: Option<u32>,
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

pub(super) struct ActiveDispatch {
    pub(super) dispatch_id: u64,
    pub(super) module: ModuleId,
    pub(super) route: String,
    /// For a dispatch that runs an idle instance's timers, when the instance
    /// stops unless a request arrives first.
    pub(super) idle_stop_at: Option<tokio::time::Instant>,
    /// Set when the host first sees the dispatch's cancellation request: the
    /// dispatch is killed if it is still here then.
    pub(super) grace_deadline: Option<tokio::time::Instant>,
    pub(super) responder: Option<super::dispatch_api::Responder>,
    pub(super) completion: Arc<DispatchCompletion>,
}

/// How many dispatches an instance runs at once. An instance runs one slice at
/// a time, so dispatches that mostly wait share it well and ones that mostly
/// compute delay each other; past this a dispatch gets another instance.
pub(super) const DISPATCHES_PER_INSTANCE: usize = 8;

/// The dispatches in progress on one instance, and the ones that have joined
/// it and wait for their first slice.
#[derive(Default)]
pub(super) struct ActiveDispatches(Vec<ActiveDispatch>, usize);

impl ActiveDispatches {
    pub(super) fn get(&self, dispatch: u64) -> Option<&ActiveDispatch> {
        self.0.iter().find(|active| active.dispatch_id == dispatch)
    }

    pub(super) fn get_mut(&mut self, dispatch: u64) -> Option<&mut ActiveDispatch> {
        self.0
            .iter_mut()
            .find(|active| active.dispatch_id == dispatch)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &ActiveDispatch> {
        self.0.iter()
    }

    pub(super) fn into_inner(self) -> Vec<ActiveDispatch> {
        self.0
    }

    fn count(&self) -> usize {
        self.0.len() + self.1
    }
}

impl Cell {
    /// Starts tracking a dispatch on its instance; `joined` releases the place
    /// [`Cell::join_shared`] reserved for it.
    pub(super) fn insert_active(
        &self,
        instance: InstanceId,
        dispatch: ActiveDispatch,
        joined: bool,
    ) {
        self.active.upsert(instance, |active| {
            active.0.push(dispatch);
            if joined {
                active.1 -= 1;
            }
        });
    }

    /// Releases a joined dispatch's reserved place without it having begun.
    pub(super) fn release_reservation(&self, instance: InstanceId) -> usize {
        self.active
            .with_mut_or_remove(
                &instance,
                |active| {
                    active.1 -= 1;
                    active.count()
                },
                |active| active.count() == 0,
            )
            .unwrap_or(0)
    }

    /// An instance of `entry` that already has dispatches in progress and room
    /// for another, with a place reserved for it, so the instance is not
    /// released before the dispatch begins.
    pub(super) fn join_shared(&self, entry: &ModuleEntry) -> Option<Arc<Slot>> {
        let mut shared = entry.shared.lock();
        let mut joined = None;
        shared.retain(|candidate| {
            let Some(slot) = candidate.upgrade() else {
                return false;
            };
            if joined.is_some() {
                return true;
            }
            if slot.state() == LifecycleTier::Discarded {
                return false;
            }
            let reserved = self.active.with_mut(&slot.id, |active| {
                let room = active.count() < DISPATCHES_PER_INSTANCE;
                if room {
                    active.1 += 1;
                }
                room
            });
            match reserved {
                None => false,
                Some(true) => {
                    joined = Some(slot);
                    true
                }
                Some(false) => true,
            }
        });
        joined
    }

    /// Offers an instance that has begun a dispatch to later dispatches.
    pub(super) fn share(&self, entry: &ModuleEntry, slot: &Arc<Slot>) {
        let mut shared = entry.shared.lock();
        if !shared
            .iter()
            .any(|candidate| std::ptr::eq(candidate.as_ptr(), Arc::as_ptr(slot)))
        {
            shared.push(Arc::downgrade(slot));
        }
    }

    /// Runs `f` on one of `instance`'s dispatches in progress.
    pub(super) fn with_active<R>(
        &self,
        instance: InstanceId,
        dispatch: u64,
        f: impl FnOnce(&ActiveDispatch) -> R,
    ) -> Option<R> {
        self.active
            .with(&instance, |active| active.get(dispatch).map(f))
            .flatten()
    }

    pub(super) fn with_active_mut<R>(
        &self,
        instance: InstanceId,
        dispatch: u64,
        f: impl FnOnce(&mut ActiveDispatch) -> R,
    ) -> Option<R> {
        self.active
            .with_mut(&instance, |active| active.get_mut(dispatch).map(f))
            .flatten()
    }

    /// Runs `f` on the earliest of `instance`'s dispatches in progress.
    pub(super) fn first_active<R>(
        &self,
        instance: InstanceId,
        f: impl FnOnce(&ActiveDispatch) -> R,
    ) -> Option<R> {
        self.active
            .with(&instance, |active| active.iter().next().map(f))
            .flatten()
    }

    pub(super) fn active_dispatch_ids(&self, instance: InstanceId) -> Vec<u64> {
        self.active
            .with(&instance, |active| {
                active.iter().map(|dispatch| dispatch.dispatch_id).collect()
            })
            .unwrap_or_default()
    }

    /// Removes a dispatch, and returns it with how many remain on the
    /// instance.
    pub(super) fn remove_active(
        &self,
        instance: InstanceId,
        dispatch: u64,
    ) -> (Option<ActiveDispatch>, usize) {
        self.active
            .with_mut_or_remove(
                &instance,
                |active| {
                    let removed = active
                        .0
                        .iter()
                        .position(|candidate| candidate.dispatch_id == dispatch)
                        .map(|index| active.0.remove(index));
                    (removed, active.count())
                },
                |active| active.count() == 0,
            )
            .unwrap_or((None, 0))
    }

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

    /// Registers a local debug guest with no idle pooling or capture, so each
    /// dispatch activates a new guest. The caller must use the local debug wall
    /// backstop for its cell.
    pub fn register_debug_module(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<crate::debug::DebugActivator>,
        manifest: AppManifest,
    ) -> crate::Result<()> {
        anyhow::ensure!(
            self.cfg.slice_wall_ms_cap == crate::debug::DEBUG_BUDGET_MS,
            "local debugging requires its explicit cell wall backstop"
        );
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
        debug_occupancy_ms: Option<u32>,
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
        let entry = ModuleEntry {
            shared: Mutex::new(Vec::new()),
            tenant: tenant.to_string(),
            limits,
            debug_occupancy_ms,
            manifest,
            pool: InstancePool::new(
                activator.clone(),
                if debug_occupancy_ms.is_some() {
                    0
                } else {
                    self.cfg.max_idle_per_module
                },
            ),
            activator,
            meters: Arc::new(Meters::new()),
            endpoint_exceptions,
        };
        entry
            .pool
            .map_activation(entry.manifest.memory_max_bytes().try_into()?)?;
        self.modules.write().insert(module, Arc::new(entry));
        Ok(())
    }

    /// Stops admitting work for `module` and discards its idle instances. Work
    /// already running holds the module until it finishes. Returns whether the
    /// module was registered.
    pub fn retire_module(&self, module: &ModuleId) -> bool {
        let Some(entry) = self.modules.write().remove(module) else {
            return false;
        };
        entry.pool.retire();
        true
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
                allowlist: entry.manifest.bindings.fetch_allowlist.clone(),
            },
            entry.meters.clone(),
            Some(self.rt.clone()),
        );
        context.limits = entry.manifest.effective_limits();
        context.endpoint_exceptions = entry.endpoint_exceptions.clone();
        context.development_notices = self.cfg.development_notices;
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

    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        self.residents.lock().get(&id).map(|r| r.slot.clone())
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
