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

    /// The capture sequence, then the store write.
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
                } else if slot.state() == LifecycleTier::Discarded {
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
        let capture = crate::hibernate_capture::capture_in_context(
            slot,
            self.capture_env(entry, slot),
        );
        let res = match capture {
            Ok(r) => r,
            Err(hibernate_policy::CaptureRefused::SnapshotWrite(e)) => {
                self.metrics
                    .capture_write_failures
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    tenant = %entry.tenant, instance = %slot.id, error = %e,
                    "hibernation snapshot write failed; the instance stays Parked and no capture is \
                     counted"
                );
                return Err(hibernate_policy::CaptureRefused::SnapshotWrite(e));
            }
            Err(e) => {
                // A kill inside the gc hint is nowhere near the dispatch path;
                // `capture` made the slot match the engine and the books are
                // this half.
                if let hibernate_policy::CaptureRefused::GcHintDidNotComplete(c) = e {
                    if !c.instance_survives() {
                        self.record_offpath_kill(slot, entry, c);
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

        let bytes = res.saved.bytes;
        slot.hibernate()
            .map_err(|e| hibernate_policy::CaptureRefused::Failed(format!("{e:?}")))?;

        entry.meters.record_capture(bytes);
        self.metrics.captures.fetch_add(1, Ordering::Relaxed);
        self.metrics.delta_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.metrics.capture_us.record(res.capture_us);
        self.metrics.gc_hint_us.record(res.gc_hint_us);
        self.metrics
            .dirty_pages
            .record(res.saved.changed_pages as u64);
        tracing::info!(target: "isoplane_host::cell::transition",
            event = "instance_transition", tenant = %entry.tenant,
            app = %entry.activator.module().app, version = %entry.activator.module().version,
            instance = slot.id.0, previous = "parked", current = "hibernated",
            cause = "capture", snapshot_bytes = bytes);
        if bytes > self.cfg.hibernate.delta_advisory_bytes {
            tracing::warn!(
                tenant = %entry.tenant, instance = %slot.id, delta_bytes = bytes,
                "delta above the advisory threshold"
            );
        }
        Ok(bytes)
    }

    /// Activates from the template, applies the delta, rebinds the manifest's
    /// handles, and delivers the losses the capture promised.
    #[tracing::instrument(name = "restore", skip_all, fields(tenant = %entry.tenant,
        app = %entry.activator.module().app, version = %entry.activator.module().version,
        instance = %slot.id))]
    pub(super) fn wake(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<()> {
        if slot.state() != LifecycleTier::Hibernated {
            anyhow::bail!("instance {} is not hibernated", slot.id)
        }
        let expect = isoplane_hibernate::Binding::new(
            entry.activator.module_hash(),
            entry.activator.abi_rev(),
        );
        let snapshot = tracing::info_span!("delta_store_load", tier = "reserved").in_scope(|| {
            match slot.ctx.snapshot.lock().as_mut() {
                Some(reservation) => {
                    isoplane_hibernate::local::Snapshot::open(reservation.as_mut(), expect)
                }
                None => Err(anyhow::anyhow!(
                    "instance {} has no snapshot reservation",
                    slot.id
                )),
            }
        });
        let dormant = slot.take_dormant();
        let activate = |dormant: Option<Box<dyn isoplane_engine::DormantInstance>>| {
            tracing::info_span!("restore_activate").in_scope(|| {
                let bindings = crate::bindings::CellBindings::boxed(slot.ctx.clone());
                match dormant {
                    Some(mapped) => mapped.activate(bindings),
                    None => entry.pool.activate(bindings),
                }
            })
        };
        let restored = match snapshot {
            Ok(snapshot) => {
                let mut inst = activate(dormant)?;
                let applied = inst.restore_mutable_state(snapshot.image()).and_then(|()| {
                    tracing::info_span!("delta_apply").in_scope(|| {
                        let mut reservation = slot.ctx.snapshot.lock();
                        let reservation = reservation.as_mut().ok_or_else(|| {
                            anyhow::anyhow!("instance {} lost its snapshot reservation", slot.id)
                        })?;
                        snapshot.apply(reservation.as_mut(), inst.memory())
                    })
                });
                applied.map(|()| inst)
            }
            Err(e) => {
                drop(dormant);
                Err(e)
            }
        };
        let mut fell_back = false;
        let inst = match restored {
            Ok(inst) => {
                inst
            }
            Err(e) => {
                if self.active.contains_key(&slot.id) {
                    return Err(e.context("a pending dispatch requires its captured continuation"));
                }
                tracing::warn!(instance = %slot.id, error = %e, "delta unusable; falling back to template");
                fell_back = true;
                activate(None)?
            }
        };
        if fell_back {
            self.metrics.delta_fallbacks.fetch_add(1, Ordering::Relaxed);
        }

        slot.rehydrate(inst);
        tracing::info!(target: "isoplane_host::cell::transition",
            event = "instance_transition", tenant = %entry.tenant,
            app = %entry.activator.module().app, version = %entry.activator.module().version,
            instance = slot.id.0, previous = "hibernated", current = "parked",
            cause = if fell_back { "template_fallback" } else { "restore" });
        Ok(())
    }

    /// Captures now, bypassing the idle trajectory's timing and none of its
    /// eligibility rules. Blocks on the cell's reactor for the delta write,
    /// so it is called from a blocking task, never a reactor thread.
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

    /// Wakes from a delta and queues any results that came due while asleep.
    /// Blocks on the reactor like [`Cell::force_hibernate`].
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

    /// Discards an idle instance without saving a delta.
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
