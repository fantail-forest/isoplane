//! The idle trajectory and the forced transitions: capture, wake, discard.

use super::*;

impl Cell {
    /// One pass of the idle trajectory. The first pass over every resident
    /// reads only atomics and clones nothing, because the sweep runs several
    /// times a second over a cell full of parked instances and a parked
    /// instance is supposed to cost no CPU.
    pub(super) fn sweep_idle(self: &Arc<Self>) {
        self.sweep_handler_pressure();
    }

    pub(super) fn capture_env<'a>(
        &'a self,
        entry: &'a Arc<ModuleEntry>,
        slot: &Slot,
    ) -> hibernate_policy::CaptureEnv<'a> {
        hibernate_policy::CaptureEnv {
            dispatch_active: self.active.contains_key(&slot.id),
            observer: self.active.with(&slot.id, |request| {
                request.completion.clone() as Arc<dyn hibernate_policy::CaptureObserver>
            }),
            activator: &entry.activator,
        }
    }

    /// Captures into locally owned storage before releasing resident pages.
    pub(super) fn hibernate_now(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
    ) -> Result<u64, hibernate_policy::CaptureRefused> {
        let owner = self
            .active
            .with(&slot.id, |request| request.completion.clone());
        let capture = || {
            let _stage = tracing::info_span!("capture", tenant = %entry.tenant,
                app = %entry.activator.module().app, version = %entry.activator.module().version,
                instance = %slot.id)
            .entered();
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.capture_owned(slot, entry)
            }))
        };
        let result = match &owner {
            Some(owner) => owner.in_scope(capture),
            None => capture(),
        };
        match result {
            Ok(result) => result,
            Err(_) => {
                self.note_caught_panic("capture");
                if let Some(owner) = owner {
                    self.end_failed_capture(slot, entry, &owner, TerminalReason::HostPanic);
                } else if slot.is_tombstoned() {
                    slot.discard();
                    self.remember_tombstone(slot);
                    self.forget(slot);
                }
                Err(hibernate_policy::CaptureRefused::Failed(
                    "host panic during capture".into(),
                ))
            }
        }
    }

    fn capture_owned(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
    ) -> Result<u64, hibernate_policy::CaptureRefused> {
        let capture =
            crate::hibernate_capture::capture_in_context(slot, self.capture_env(entry, slot));
        let res = match capture {
            Ok(result) => result,
            Err(hibernate_policy::CaptureRefused::SnapshotWrite(error)) => {
                self.metrics
                    .capture_write_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(tenant = %entry.tenant, instance = %slot.id, error = %error,
                    "local snapshot publication failed; the instance remains resident");
                return Err(hibernate_policy::CaptureRefused::SnapshotWrite(error));
            }
            Err(e) => {
                // A tombstone raised inside the gc hint is nowhere near the
                // dispatch path; `capture` made the slot match the engine and
                // the books are this half.
                if let hibernate_policy::CaptureRefused::GcHintDidNotComplete(c) = e {
                    if c.tombstones() {
                        self.record_offpath_tombstone(slot, entry, c);
                        if let Some(owner) = self
                            .active
                            .with(&slot.id, |request| request.completion.clone())
                        {
                            self.end_failed_capture(
                                slot,
                                entry,
                                &owner,
                                TerminalReason::CaptureFailed,
                            );
                        }
                    }
                }
                return Err(e);
            }
        };
        let bytes = res.bytes;
        slot.hibernate()
            .map_err(|error| hibernate_policy::CaptureRefused::Failed(format!("{error:?}")))?;
        entry.meters.record_capture(bytes);
        self.metrics.captures.fetch_add(1, Ordering::Relaxed);
        self.metrics.delta_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.metrics.capture_us.record(res.capture_us);
        self.metrics.gc_hint_us.record(res.gc_hint_us);
        self.metrics.dirty_pages.record(res.changed_pages as u64);
        if bytes > self.cfg.hibernate.delta_advisory_bytes {
            tracing::warn!(
                tenant = %entry.tenant, instance = %slot.id, delta_bytes = bytes,
                "snapshot above the advisory threshold"
            );
        }
        Ok(bytes)
    }

    /// Activates from the template and restores sandbox state in its original host context.
    #[tracing::instrument(name = "restore", skip_all, fields(tenant = %entry.tenant,
        app = %entry.activator.module().app, version = %entry.activator.module().version,
        instance = %slot.id))]
    pub(super) fn wake(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<()> {
        if slot.state() != LifecycleTier::Hibernated {
            anyhow::bail!("instance {} is not hibernated", slot.id);
        }
        let snapshot = crate::hibernate_capture::read_in_context(slot, &entry.activator);
        let mut instance = tracing::info_span!("restore_activate").in_scope(|| {
            entry
                .activator
                .activate(crate::bindings::CellBindings::boxed(slot.ctx.clone()))
        })?;
        let mut fell_back = false;
        match snapshot {
            Ok(snapshot) => {
                instance.restore_mutable_state(snapshot.image())?;
                tracing::info_span!("delta_apply")
                    .in_scope(|| snapshot.apply(instance.memory()))?;
            }
            Err(error) => {
                if self.active.contains_key(&slot.id) {
                    return Err(
                        error.context("a pending dispatch requires its captured continuation")
                    );
                }
                tracing::warn!(instance = %slot.id, error = %error, "local snapshot unusable; falling back to template");
                fell_back = true;
            }
        }
        if fell_back {
            self.metrics.delta_fallbacks.fetch_add(1, Ordering::Relaxed);
        }
        slot.rehydrate(instance);

        Ok(())
    }

    /// Captures now, bypassing the idle trajectory's timing and none of its
    /// eligibility rules. Writes the local snapshot on the calling execution thread.
    pub fn force_hibernate(
        self: &Arc<Self>,
        id: InstanceId,
    ) -> Result<u64, hibernate_policy::CaptureRefused> {
        let (slot, module) = {
            let g = self.residents.lock();
            let r = g.get(&id).ok_or_else(|| {
                hibernate_policy::CaptureRefused::Failed("no such instance".into())
            })?;
            (r.slot.clone(), r.module.clone())
        };
        let entry = self
            .modules
            .read()
            .get(&module)
            .cloned()
            .ok_or_else(|| hibernate_policy::CaptureRefused::Failed("no such module".into()))?;
        let _execution = slot.lock_execution();
        self.hibernate_now(&slot, &entry)
    }

    /// Restores local sandbox state and queues results that came due while asleep.
    pub fn force_wake(self: &Arc<Self>, id: InstanceId) -> crate::Result<()> {
        let (slot, module) = {
            let g = self.residents.lock();
            let r = g
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("no such instance"))?;
            (r.slot.clone(), r.module.clone())
        };
        let entry = self
            .modules
            .read()
            .get(&module)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no such module"))?;
        let _execution = slot.lock_execution();
        if slot.state() != LifecycleTier::Hibernated {
            return Ok(());
        }
        let t0 = Instant::now();
        self.wake(&slot, &entry)?;
        self.metrics
            .hibernation_wake_us
            .record(t0.elapsed().as_micros() as u64);
        self.metrics
            .hibernation_wakes
            .fetch_add(1, Ordering::Relaxed);
        entry.meters.record_hibernation_wake();
        if self.rt.futures.has_ready(slot.id) {
            self.queue_wake(slot.clone());
        }
        Ok(())
    }

    /// Discards an idle instance after releasing its host resources.
    pub fn discard_instance(self: &Arc<Self>, id: InstanceId) -> crate::Result<()> {
        let slot = self
            .slot(id)
            .ok_or_else(|| anyhow::anyhow!("no such instance {id}"))?;
        let _execution = slot.try_lock_execution().ok_or_else(|| {
            anyhow::anyhow!("instance {id} is executing; eviction requires an idle boundary")
        })?;
        if self.active.contains_key(&id) {
            anyhow::bail!(
                "instance {id} owns an unfinished dispatch; eviction would lose its continuation"
            );
        }
        if slot.state() == LifecycleTier::Hot {
            anyhow::bail!("instance {id} is Hot; eviction is from Parked, not from a live stack");
        }
        self.rt.futures.release_instance(id);
        self.forget(&slot);
        slot.discard();
        Ok(())
    }
}
