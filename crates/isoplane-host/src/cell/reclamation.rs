//! Reclaimable instance ownership and pressure-driven local policy.

use super::*;

#[cfg(test)]
mod tests;

/// Priority class of an eligible instance.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ReclaimKind {
    /// Completed request with resident memory retained for reuse.
    Idle,
    /// Unfinished request parked at a returned-stack boundary.
    Active,
    /// Completed request whose cached snapshot can be discarded.
    HibernatedIdle,
}

/// A candidate that must be revalidated by its owning cell before reclamation.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ReclaimCandidate {
    /// Identity within the owning cell.
    pub instance: InstanceId,
    /// Last activity boundary in Unix milliseconds: the candidate's place
    /// in least-recently-used order.
    pub last_used_ms: u64,
    /// The instance's park count when it was listed. Reclamation is refused
    /// once the instance has parked again.
    pub parks: u64,
    /// The permitted reclamation action.
    pub kind: ReclaimKind,
}

impl Cell {
    /// Connects admission to the node's global idle-snapshot eviction coordinator.
    /// The callback runs on a blocking caller without cell locks held.
    pub fn set_storage_reclaimer(
        &self,
        reclaimer: Arc<dyn Fn() -> crate::Result<bool> + Send + Sync>,
    ) {
        *self.storage_reclaimer.write() = Some(reclaimer);
    }

    pub(super) fn reserve_snapshot(
        self: &Arc<Self>,
        bytes: isoplane_hibernate::snapshot::SnapshotBytes,
    ) -> Result<
        Box<dyn isoplane_hibernate::snapshot::SnapshotReservation>,
        isoplane_hibernate::snapshot::SnapshotError,
    > {
        let mut failure = match self.snapshots.reserve(bytes) {
            Ok(reservation) => return Ok(reservation),
            Err(error) => error,
        };
        if !failure.is_capacity_exhausted() {
            return Err(failure);
        }
        if self.node_coordinated_reclamation.load(Ordering::Acquire) {
            let reclaimer = self.storage_reclaimer.read().clone();
            if let Some(reclaimer) = reclaimer {
                loop {
                    match reclaimer() {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(error) => {
                            tracing::warn!(cell = %self.cfg.name, %error, "snapshot capacity reclamation failed");
                            break;
                        }
                    }
                    match self.snapshots.reserve(bytes) {
                        Ok(reservation) => return Ok(reservation),
                        Err(error) if error.is_capacity_exhausted() => failure = error,
                        Err(error) => return Err(error),
                    }
                }
            }
            return Err(failure);
        }
        for candidate in self
            .reclamation_candidates(crate::now_ms())
            .into_iter()
            .filter(|candidate| candidate.kind == ReclaimKind::HibernatedIdle)
        {
            match self.reclaim_candidate(candidate) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(instance = %candidate.instance, %error, "idle snapshot eviction failed");
                    continue;
                }
            }
            match self.snapshots.reserve(bytes) {
                Ok(reservation) => return Ok(reservation),
                Err(error) if error.is_capacity_exhausted() => failure = error,
                Err(error) => return Err(error),
            }
        }
        Err(failure)
    }

    /// Gives the node coordinator exclusive ownership of ordinary-handler reclamation.
    pub fn set_node_coordinated_reclamation(&self) {
        self.node_coordinated_reclamation
            .store(true, Ordering::Release);
    }

    /// Lists candidates without taking ownership; concurrent activity can invalidate them.
    pub fn reclamation_candidates(&self, now_ms: u64) -> Vec<ReclaimCandidate> {
        let mut candidates: Vec<_> = self
            .residents
            .lock()
            .values()
            .filter_map(|resident| {
                let slot = &resident.slot;
                let active = self.active.contains_key(&slot.id);
                let kind = match (slot.state(), active) {
                    (LifecycleTier::Parked, false) => ReclaimKind::Idle,
                    (LifecycleTier::Parked, true) => ReclaimKind::Active,
                    (LifecycleTier::Hibernated, false) => ReclaimKind::HibernatedIdle,
                    (LifecycleTier::Hot | LifecycleTier::Discarded, _)
                    | (LifecycleTier::Hibernated, true) => return None,
                };
                let last_used_ms = slot.last_used_ms();
                if kind != ReclaimKind::HibernatedIdle
                    && now_ms.saturating_sub(last_used_ms) < self.cfg.hibernate.handler_after_ms
                {
                    return None;
                }
                Some(ReclaimCandidate {
                    instance: slot.id,
                    last_used_ms,
                    parks: slot.parks(),
                    kind,
                })
            })
            .collect();
        candidates
            .sort_by_key(|candidate| (candidate.kind, candidate.last_used_ms, candidate.instance));
        candidates
    }

    /// Reclaims one candidate if it still has the requested ownership and lifecycle state.
    /// Disk I/O is synchronous; call from an execution or blocking thread.
    pub fn reclaim_candidate(self: &Arc<Self>, candidate: ReclaimCandidate) -> crate::Result<bool> {
        let ReclaimCandidate {
            instance: id,
            kind,
            parks,
            last_used_ms: _,
        } = candidate;
        let Some((slot, module)) = self
            .residents
            .lock()
            .get(&id)
            .map(|resident| (resident.slot.clone(), resident.module.clone()))
        else {
            return Ok(false);
        };
        let Some(entry) = self.modules.read().get(&module).cloned() else {
            return Ok(false);
        };
        let idle_claim = if kind == ReclaimKind::Active {
            None
        } else {
            let Some(claim) = entry.pool.claim_idle(id) else {
                return Ok(false);
            };
            Some(claim)
        };
        let slot = idle_claim
            .as_ref()
            .map(|claim| claim.slot().clone())
            .unwrap_or(slot);
        let Some(_execution) = slot.try_lock_execution() else {
            return Ok(false);
        };
        if slot.parks() != parks {
            return Ok(false);
        }
        if self.active.contains_key(&id) != (kind == ReclaimKind::Active) {
            return Ok(false);
        }
        let expected = if kind == ReclaimKind::HibernatedIdle {
            LifecycleTier::Hibernated
        } else {
            LifecycleTier::Parked
        };
        if slot.state() != expected {
            return Ok(false);
        }
        match kind {
            ReclaimKind::HibernatedIdle => {
                slot.discard();
                self.forget(&slot);
                tracing::info!(target: "isoplane_host::cell::transition",
                    event = "instance_transition", tenant = %entry.tenant,
                    app = %module.app, version = %module.version,
                    instance = slot.id.0, previous = "hibernated", current = "discarded",
                    cause = "storage_pressure");
            }
            ReclaimKind::Idle | ReclaimKind::Active => {
                self.hibernate_now(&slot, &entry)
                    .map_err(|error| anyhow::anyhow!("instance {id} capture refused: {error:?}"))?;
            }
        }
        drop(idle_claim);
        Ok(true)
    }

    pub(super) fn sweep_handler_pressure(self: &Arc<Self>) {
        if self.node_coordinated_reclamation.load(Ordering::Acquire) {
            return;
        }
        for candidate in self.reclamation_candidates(crate::now_ms()) {
            if candidate.kind == ReclaimKind::HibernatedIdle {
                continue;
            }
            let sample = match self.memory_pressure.read().sample() {
                Ok(sample) => sample,
                Err(error) => {
                    tracing::warn!(cell = %self.cfg.name, %error, "cannot sample memory pressure");
                    return;
                }
            };
            let reclaim = self
                .cfg
                .hibernate
                .pressure_requested(sample, self.reclaiming.load(Ordering::Relaxed));
            self.reclaiming.store(reclaim, Ordering::Relaxed);
            if !reclaim {
                return;
            }
            if let Err(error) = self.reclaim_candidate(candidate) {
                tracing::debug!(instance = %candidate.instance, %error, "pressure capture refused");
            }
        }
    }
}
