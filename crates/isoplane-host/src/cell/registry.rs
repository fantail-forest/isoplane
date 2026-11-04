//! Module registration and the resident-instance bookkeeping tables.

use super::*;
use crate::DISPATCHES_PER_INSTANCE;

#[derive(Hash, PartialEq, Eq)]
pub(super) struct ApplicationKey {
    tenant: String,
    app: String,
}

pub(super) struct ModuleEntry {
    pub(super) instances: Arc<crate::admission::ApplicationInstances>,
    /// Instances that have had dispatches in progress, which a new dispatch
    /// may join; pruned as they are found to have none.
    pub(super) shared: Mutex<Vec<Weak<Slot>>>,
    pub(super) tenant: String,
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
    /// Set when the host first sees the dispatch's cancellation request: the
    /// dispatch is killed if it is still here then.
    pub(super) grace_deadline: Option<tokio::time::Instant>,
    pub(super) responder: Option<super::dispatch_api::Responder>,
    pub(super) completion: Arc<DispatchCompletion>,
}

/// The dispatches in progress on one instance, and the ones that have joined
/// it and wait for their first slice.
#[derive(Default)]
pub(super) struct ActiveDispatches {
    dispatches: Vec<ActiveDispatch>,
    joining: usize,
    /// Since when one of the dispatches has been in the grace after its
    /// cancellation. The instance takes no new dispatches meanwhile, since a
    /// kill at the end of the grace would fail them too.
    held_since: Option<Instant>,
}

impl ActiveDispatches {
    pub(super) fn get(&self, dispatch: u64) -> Option<&ActiveDispatch> {
        self.dispatches
            .iter()
            .find(|active| active.dispatch_id == dispatch)
    }

    pub(super) fn get_mut(&mut self, dispatch: u64) -> Option<&mut ActiveDispatch> {
        self.dispatches
            .iter_mut()
            .find(|active| active.dispatch_id == dispatch)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &ActiveDispatch> {
        self.dispatches.iter()
    }

    pub(super) fn into_inner(self) -> Vec<ActiveDispatch> {
        self.dispatches
    }

    fn count(&self) -> usize {
        self.dispatches.len() + self.joining
    }
}

/// Whether a dispatch found an instance already running others to join.
pub(super) enum Join {
    Joined(Arc<Slot>),
    /// None had room. `held` is whether one would have, but for a dispatch
    /// in its grace.
    Unavailable {
        held: bool,
    },
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
            active.dispatches.push(dispatch);
            if joined {
                active.joining -= 1;
            }
        });
    }

    /// Releases a joined dispatch's reserved place without it having begun.
    pub(super) fn release_reservation(&self, instance: InstanceId) -> usize {
        self.active
            .with_mut_or_remove(
                &instance,
                |active| {
                    active.joining -= 1;
                    active.count()
                },
                |active| active.count() == 0,
            )
            .unwrap_or(0)
    }

    /// An instance of `entry` that already has dispatches in progress and room
    /// for another, with a place reserved for it, so the instance is not
    /// released before the dispatch begins. Instances are tried oldest first.
    pub(super) fn join_shared(&self, entry: &ModuleEntry, allow_queued: bool) -> Join {
        let mut shared = entry.shared.lock();
        let mut joined = None;
        let mut held = false;
        shared.retain(|candidate| {
            let Some(slot) = candidate.upgrade() else {
                return false;
            };
            if joined.is_some() {
                return true;
            }
            if slot.state() == LifecycleTier::Discarded || slot.is_retiring() {
                return false;
            }
            // A slice may be running; one already waiting for the thread means
            // the instance is behind, and a new dispatch would wait too.
            let behind = self.has_runnable(&slot) || self.starts.contains_key(&slot.id);
            let reserved = self.active.with_mut(&slot.id, |active| {
                let room = active
                    .dispatches
                    .iter()
                    .filter(|dispatch| dispatch.dispatch_id != crate::futures::INSTANCE_WORK)
                    .count()
                    + active.joining
                    < DISPATCHES_PER_INSTANCE;
                held |= room && active.held_since.is_some();
                let join = room
                    && active.held_since.is_none()
                    && (!behind || allow_queued)
                    && slot.has_memory_for_join();
                if join {
                    active.joining += 1;
                }
                join
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
        match joined {
            Some(slot) => Join::Joined(slot),
            None => Join::Unavailable { held },
        }
    }

    /// Starts a dispatch's grace, once, holding its instance from new
    /// dispatches. Returns whether this call started it.
    pub(super) fn start_grace(
        &self,
        instance: InstanceId,
        dispatch: u64,
        deadline: tokio::time::Instant,
    ) -> bool {
        self.active
            .with_mut(&instance, |active| {
                let Some(started) =
                    active
                        .get_mut(dispatch)
                        .and_then(|dispatch| match dispatch.grace_deadline {
                            Some(_) => None,
                            None => {
                                dispatch.grace_deadline = Some(deadline);
                                Some(dispatch.completion.entry.meters.clone())
                            }
                        })
                else {
                    return false;
                };
                if active.held_since.is_none() {
                    active.held_since = Some(Instant::now());
                    self.metrics.instance_holds.fetch_add(1, Ordering::Relaxed);
                    started.record_instance_hold();
                }
                true
            })
            .unwrap_or(false)
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
                        .dispatches
                        .iter()
                        .position(|candidate| candidate.dispatch_id == dispatch)
                        .map(|index| active.dispatches.remove(index));
                    if let (Some(removed), Some(since)) = (&removed, active.held_since) {
                        if active.iter().all(|other| other.grace_deadline.is_none()) {
                            active.held_since = None;
                            let held_ms = since.elapsed().as_millis() as u64;
                            self.metrics.instance_hold_ms.record(held_ms);
                            removed
                                .completion
                                .entry
                                .meters
                                .record_instance_hold_ms(held_ms);
                        }
                    }
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
        let module = activator.module().clone();
        let instances = {
            let mut applications = self.applications.lock();
            applications.retain(|_, instances| instances.strong_count() > 0);
            let key = ApplicationKey {
                tenant: tenant.to_owned(),
                app: module.app.clone(),
            };
            if let Some(instances) = applications.get(&key).and_then(Weak::upgrade) {
                instances
            } else {
                let instances = Arc::new(crate::admission::ApplicationInstances::new());
                applications.insert(key, Arc::downgrade(&instances));
                instances
            }
        };
        let entry = ModuleEntry {
            instances,
            shared: Mutex::new(Vec::new()),
            tenant: tenant.to_string(),
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
            meters: Arc::new(Meters::for_tenant(self.metrics.tenant_accounting(tenant))),
            endpoint_exceptions,
        };
        entry
            .pool
            .map_activation(entry.manifest.memory_max_bytes().try_into()?)?;
        let replaced = self.modules.write().insert(module, Arc::new(entry));
        if let Some(replaced) = replaced {
            replaced.pool.retire();
        }
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
                limits: entry.manifest.effective_limits(),
                allowlist: entry.manifest.bindings.fetch_allowlist.clone(),
            },
            entry.meters.clone(),
            Some(self.rt.clone()),
        );
        context.endpoint_exceptions = entry.endpoint_exceptions.clone();
        context.configure_resources(self.cfg.guest_resources);
        Arc::new(context)
    }

    pub(super) fn acquire_handler(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<(Arc<Slot>, AcquireSource)> {
        let mut replacing_snapshot = false;
        let (slot, src) = entry.pool.acquire_reserved(
            |id| {
                let limit = crate::admission::InstanceLimit(
                    entry.manifest.effective_limits().instances_per_application,
                );
                let admission = entry.instances.reserve(limit).or_else(|error| {
                    if limit.0 > 0 && self.discard_idle_sibling(entry) {
                        entry.instances.reserve(limit)
                    } else {
                        Err(error)
                    }
                })?;
                let ctx = self.make_ctx(entry, id);
                *ctx.admission.lock() = Some(admission);
                let maximum = entry.activator.snapshot_size_bound(
                    isoplane_abi::config::memory_ceiling_bytes(ctx.limits.instance_memory_max),
                )?;
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
            |slot| {
                let result = self.wake(slot, entry);
                replacing_snapshot |= result
                    .as_ref()
                    .is_err_and(|error| error.is::<isoplane_hibernate::WakeFallback>());
                result
            },
        )?;
        if replacing_snapshot {
            self.metrics.delta_fallbacks.fetch_add(1, Ordering::Relaxed);
        }
        self.index.lock().insert(slot.id, Arc::downgrade(&slot));
        let mut residents = self.residents.lock();
        residents.entry(slot.id).or_insert_with(|| Resident {
            slot: slot.clone(),
            module: entry.activator.module().clone(),
        });
        drop(residents);
        Ok((slot, src))
    }

    fn discard_idle_sibling(&self, entry: &Arc<ModuleEntry>) -> bool {
        let siblings: Vec<_> = self
            .modules
            .read()
            .values()
            .filter(|other| {
                !Arc::ptr_eq(other, entry) && Arc::ptr_eq(&other.instances, &entry.instances)
            })
            .cloned()
            .collect();
        for sibling in siblings {
            let mut slots: Vec<_> = self
                .residents
                .lock()
                .values()
                .filter(|resident| &resident.module == sibling.activator.module())
                .map(|resident| resident.slot.clone())
                .collect();
            slots.sort_by_key(|slot| slot.last_used_ms());
            for slot in slots {
                let Some(_execution) = slot.try_lock_execution() else {
                    continue;
                };
                if self.active.contains_key(&slot.id) {
                    continue;
                }
                if let Some(idle) = sibling.pool.take_idle(slot.id, |slot| {
                    matches!(
                        slot.state(),
                        LifecycleTier::Parked | LifecycleTier::Hibernated
                    )
                }) {
                    self.rt.futures.release_instance(slot.id);
                    self.forget(&slot);
                    sibling.pool.release(idle, evict());
                    return true;
                }
            }
        }
        false
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::pool::cell_tests::{cell, http, manifest};
    use crate::fakes::{FakeActivator, FakeBehaviour};

    #[test]
    fn replacing_a_registered_module_discards_its_previous_idle_guests() {
        let snapshots = tempfile::tempdir().expect("snapshot directory");
        let cell = Cell::build(
            CellConfig {
                exec_threads: 1,
                ..CellConfig::for_test("replacement")
            },
            crate::cell::pool::cell_tests::services(),
            Arc::new(
                isoplane_hibernate::snapshot::LocalSnapshotStorage::new(snapshots.path())
                    .expect("snapshot storage"),
            ),
        )
        .expect("cell without workers");
        let register = |tenant: &str, app: &str| {
            cell.register_module(
                tenant,
                Arc::new(FakeActivator::named(
                    ModuleId::new(app, "v1"),
                    FakeBehaviour::done(),
                )),
                manifest(app),
            )
        };
        let module = ModuleId::new("replaced", "v1");
        register("first", "replaced").expect("first registration");
        let response = cell
            .submit(&module, Target::Handler { route: "/".into() }, http(1))
            .expect("request");
        assert_eq!(cell.queue.depth(), 1, "one request is queued");
        let Job::Dispatch(job) = cell.queue.pop(0).expect("request job") else {
            panic!("expected request");
        };
        cell.run_dispatch(*job);
        cell.queue.job_done(0);
        let slot = cell
            .slot(response.try_recv().expect("response").instance)
            .expect("idle guest");
        assert_eq!(slot.state(), LifecycleTier::Parked);
        register("first", "replaced").expect("replacement registration");
        assert_eq!(
            slot.state(),
            LifecycleTier::Discarded,
            "replacement retires the old pool"
        );
        assert!(cell.retire_module(&module));
        register("second", "second").expect("discarded guests release capacity");
        cell.shutdown();
    }

    #[test]
    fn application_admission_counts_versions_and_releases_discarded_instances() {
        let cell = cell(CellConfig::for_test("app-cap"));
        let register = |tenant: &str, app: &str, version: &str| {
            let module = ModuleId::new(app, version);
            let mut manifest = manifest(app);
            manifest.resource_limits.instances_per_application = Some(2);
            cell.register_module(
                tenant,
                Arc::new(FakeActivator::named(module.clone(), FakeBehaviour::done())),
                manifest,
            )
            .expect("register application");
            let entry = cell.modules.read()[&module].clone();
            entry
        };
        let old = register("tenant", "app", "v1");
        let (first, _) = cell.acquire_handler(&old).expect("first activation");
        let (second, _) = cell.acquire_handler(&old).expect("second activation");
        cell.retire_module(old.activator.module());
        let new = register("tenant", "app", "v2");
        let failure = cell
            .acquire_handler(&new)
            .err()
            .expect("draining versions count");
        assert!(failure.is::<crate::admission::InstanceLimitReached>());
        let independent = register("other-tenant", "other-app", "v1");
        assert!(cell.acquire_handler(&independent).is_ok());
        let retained_context = first.ctx.clone();
        cell.forget(&first);
        old.pool.release(first, evict());
        let (replacement, _) = cell.acquire_handler(&new).expect("discard frees capacity");
        assert_ne!(replacement.id, retained_context.instance);
        new.pool.release(replacement.clone(), warm());
        let (reused, source) = cell
            .acquire_handler(&new)
            .expect("warm reuse needs no reservation");
        assert_eq!(source, AcquireSource::WarmParked);
        assert_eq!(reused.id, replacement.id);
        assert!(cell.acquire_handler(&new).is_err());
        cell.forget(&second);
        old.pool.release(second, evict());
        assert!(cell.acquire_handler(&new).is_ok());
        cell.shutdown();
    }

    #[test]
    fn a_live_version_can_replace_an_idle_sibling_at_the_application_cap() {
        let cell = cell(CellConfig::for_test("split-cap"));
        let register = |version| {
            let module = ModuleId::new("app", version);
            let mut limits = manifest("app");
            limits.resource_limits.instances_per_application = Some(1);
            cell.register_module(
                "tenant",
                Arc::new(FakeActivator::named(module.clone(), FakeBehaviour::done())),
                limits,
            )
            .expect("register live version");
            cell.modules.read()[&module].clone()
        };
        let old = register("v1");
        let new = register("v2");
        let (first, _) = cell.acquire_handler(&old).expect("first activation");
        assert!(
            cell.acquire_handler(&new).is_err(),
            "an acquired slot is not idle"
        );
        old.pool.release(first.clone(), warm());
        let (second, _) = cell.acquire_handler(&new).expect("replace idle sibling");
        assert_ne!(first.id, second.id);
        assert_eq!(first.state(), LifecycleTier::Discarded);
        assert_eq!(old.pool.live(), 0);
        assert_eq!(new.pool.live(), 1);
        assert!(cell.acquire_handler(&old).is_err(), "the cap still applies");
        cell.shutdown();
    }

    #[test]
    fn a_request_at_the_application_instance_cap_receives_service_unavailable() {
        let cell = cell(CellConfig::for_test("app-refusal"));
        let module = ModuleId::new("app", "v1");
        let mut manifest = manifest("app");
        manifest.resource_limits.instances_per_application = Some(0);
        cell.register_module(
            "tenant",
            Arc::new(FakeActivator::named(module.clone(), FakeBehaviour::done())),
            manifest,
        )
        .expect("register denied application");
        let response = cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                http(1),
                Duration::from_secs(10),
            )
            .expect("bounded admission response");
        assert_eq!(response.status, 503);
        assert_eq!(response.body, b"instance_capacity_unavailable");
        assert!(cell.resident_ids().is_empty());
        cell.shutdown();
    }
}
