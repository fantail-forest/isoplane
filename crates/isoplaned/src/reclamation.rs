//! Pressure reclamation across local cells.
use std::sync::{Arc, Weak};
use std::time::Duration;

use isoplane_host::cell::{ReclaimCandidate, ReclaimKind};
use isoplane_host::memory_pressure::{MemoryPressureSource, SystemMemoryPressure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub cell: String,
    pub instance: ReclaimCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Cursor {
    last_used_ms: u64,
    cell: String,
    instance: isoplane_host::InstanceId,
}

impl Candidate {
    fn cursor(&self) -> Cursor {
        Cursor {
            last_used_ms: self.instance.last_used_ms,
            cell: self.cell.clone(),
            instance: self.instance.instance,
        }
    }
}

#[async_trait::async_trait]
trait Participant: Send + Sync {
    fn is_alive(&self) -> bool;
    async fn candidate(
        &self,
        kind: ReclaimKind,
        now_ms: u64,
        after: Option<Cursor>,
    ) -> anyhow::Result<Option<Candidate>>;
    async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool>;
}

struct Local(Weak<crate::real::RealCells>);

#[async_trait::async_trait]
impl Participant for Local {
    fn is_alive(&self) -> bool {
        self.0.strong_count() > 0
    }

    async fn candidate(
        &self,
        kind: ReclaimKind,
        now_ms: u64,
        after: Option<Cursor>,
    ) -> anyhow::Result<Option<Candidate>> {
        Ok(self
            .0
            .upgrade()
            .and_then(|cells| local_candidate(&cells, kind, now_ms, after.as_ref())))
    }

    async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool> {
        let Some(cells) = self.0.upgrade() else {
            return Ok(false);
        };
        reclaim_local(&cells, candidate).await
    }
}



pub(crate) fn local_candidate(
    cells: &crate::real::RealCells,
    kind: ReclaimKind,
    now_ms: u64,
    after: Option<&Cursor>,
) -> Option<Candidate> {
    cells
        .cells()
        .iter()
        .flat_map(|cell| {
            cell.reclamation_candidates(now_ms)
                .into_iter()
                .filter(move |candidate| candidate.kind == kind)
                .map(move |instance| Candidate {
                    cell: cell.cfg.name.clone(),
                    instance,
                })
        })
        .filter(|candidate| after.is_none_or(|cursor| candidate.cursor() > *cursor))
        .min_by_key(Candidate::cursor)
}

pub(crate) async fn reclaim_local(
    cells: &crate::real::RealCells,
    candidate: Candidate,
) -> anyhow::Result<bool> {
    let Some(cell) = cells
        .cells()
        .iter()
        .find(|cell| cell.cfg.name == candidate.cell)
        .cloned()
    else {
        return Ok(false);
    };
    tokio::task::spawn_blocking(move || cell.reclaim_candidate(candidate.instance)).await?
}

/// Orders reclamation candidates from the host's local cells.
pub(crate) struct Coordinator {
    participants: parking_lot::Mutex<Vec<Arc<dyn Participant>>>,
    round: tokio::sync::Mutex<()>,
    pressure: Arc<dyn MemoryPressureSource>,
    policy: isoplane_host::hibernate_policy::HibernateConfig,
    reclaiming: std::sync::atomic::AtomicBool,
}

impl Coordinator {
    pub(crate) fn new(
        cells: &Arc<crate::real::RealCells>,
        policy: isoplane_host::hibernate_policy::HibernateConfig,
    ) -> Arc<Self> {
        for cell in cells.cells() {
            cell.set_node_coordinated_reclamation();
        }
        let coordinator = Arc::new(Self {
            participants: parking_lot::Mutex::new(vec![Arc::new(Local(Arc::downgrade(cells)))]),
            round: tokio::sync::Mutex::new(()),
            pressure: Arc::new(SystemMemoryPressure),
            policy,
            reclaiming: std::sync::atomic::AtomicBool::new(false),
        });
        for cell in cells.cells() {
            let weak = Arc::downgrade(&coordinator);
            let runtime = cell.reactor().clone();
            cell.set_storage_reclaimer(Arc::new(move || {
                let Some(coordinator) = weak.upgrade() else {
                    return Ok(false);
                };
                runtime.block_on(coordinator.reclaim_storage())
            }));
        }
        coordinator
    }


    pub(crate) fn start(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        interval: Duration,
        stop: tokio_util::sync::CancellationToken,
    ) {
        let weak = Arc::downgrade(self);
        runtime.spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = ticks.tick() => {},
                }
                let Some(coordinator) = weak.upgrade() else {
                    return;
                };
                let result = tokio::select! {
                    _ = stop.cancelled() => return,
                    result = coordinator.reclaim_memory() => result,
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "node memory reclamation failed");
                }
            }
        });
    }

    fn participants(&self) -> Vec<Arc<dyn Participant>> {
        let mut participants = self.participants.lock();
        participants.retain(|participant| participant.is_alive());
        participants.clone()
    }

    async fn oldest(
        participants: &[Arc<dyn Participant>],
        kind: ReclaimKind,
        now_ms: u64,
        cursors: &[Option<Cursor>],
        failed: &mut [bool],
    ) -> anyhow::Result<Option<(usize, Candidate)>> {
        let mut oldest: Option<(usize, Candidate)> = None;
        for (index, participant) in participants.iter().enumerate() {
            if failed[index] {
                continue;
            }
            let candidate = match participant
                .candidate(kind, now_ms, cursors[index].clone())
                .await
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    failed[index] = true;
                    tracing::warn!(%error, "node reclamation participant unavailable for this round");
                    continue;
                }
            };
            let Some(candidate) = candidate else { continue };
            if oldest
                .as_ref()
                .is_none_or(|(_, current)| candidate.cursor() < current.cursor())
            {
                oldest = Some((index, candidate));
            }
        }
        Ok(oldest)
    }

    #[tracing::instrument(name = "node_memory_reclamation", skip_all)]
    async fn reclaim_memory(&self) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        let participants = self.participants();
        let now_ms = isoplane_host::now_ms();
        let mut failed = vec![false; participants.len()];
        let mut idle_cursors = vec![None; participants.len()];
        let mut active_cursors = vec![None; participants.len()];
        loop {
            let _round = self.round.lock().await;
            let sample = self.pressure.sample()?;
            let reclaim = self
                .policy
                .pressure_requested(sample, self.reclaiming.load(Ordering::Relaxed));
            self.reclaiming.store(reclaim, Ordering::Relaxed);
            if !reclaim {
                return Ok(());
            }
            let (selected, cursors) = match Self::oldest(
                &participants,
                ReclaimKind::Idle,
                now_ms,
                &idle_cursors,
                &mut failed,
            )
            .await?
            {
                Some(candidate) => (Some(candidate), &mut idle_cursors),
                None => (
                    Self::oldest(
                        &participants,
                        ReclaimKind::Active,
                        now_ms,
                        &active_cursors,
                        &mut failed,
                    )
                    .await?,
                    &mut active_cursors,
                ),
            };
            let Some((index, candidate)) = selected else {
                return Ok(());
            };
            cursors[index] = Some(candidate.cursor());
            if let Err(error) = participants[index].reclaim(candidate).await {
                tracing::warn!(%error, "node reclamation candidate refused; continuing with later candidates");
            }
        }
    }

    #[tracing::instrument(name = "node_storage_reclamation", skip_all)]
    pub(crate) async fn reclaim_storage(&self) -> anyhow::Result<bool> {
        let _round = self.round.lock().await;
        let participants = self.participants();
        let now_ms = isoplane_host::now_ms();
        let mut failed = vec![false; participants.len()];
        let mut cursors = vec![None; participants.len()];
        while let Some((index, candidate)) = Self::oldest(
            &participants,
            ReclaimKind::HibernatedIdle,
            now_ms,
            &cursors,
            &mut failed,
        )
        .await?
        {
            cursors[index] = Some(candidate.cursor());
            match participants[index].reclaim(candidate).await {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "node snapshot eviction candidate refused"),
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_host::memory_pressure::{MemoryBytes, MemoryPressureError, MemorySample};

    struct Pressure;
    impl MemoryPressureSource for Pressure {
        fn sample(&self) -> Result<MemorySample, MemoryPressureError> {
            Ok(MemorySample {
                available: MemoryBytes(0),
                total: MemoryBytes(100),
            })
        }
    }

    struct TestParticipant {
        candidates: parking_lot::Mutex<Vec<Candidate>>,
        attempts: Arc<parking_lot::Mutex<Vec<u64>>>,
        refused: Option<u64>,
        unavailable: bool,
    }

    #[async_trait::async_trait]
    impl Participant for TestParticipant {
        fn is_alive(&self) -> bool {
            true
        }

        async fn candidate(
            &self,
            kind: ReclaimKind,
            _now_ms: u64,
            after: Option<Cursor>,
        ) -> anyhow::Result<Option<Candidate>> {
            anyhow::ensure!(!self.unavailable, "participant transport unavailable");
            Ok(self
                .candidates
                .lock()
                .iter()
                .filter(|candidate| candidate.instance.kind == kind)
                .filter(|candidate| {
                    after
                        .as_ref()
                        .is_none_or(|cursor| candidate.cursor() > *cursor)
                })
                .min_by_key(|candidate| candidate.cursor())
                .cloned())
        }

        async fn reclaim(&self, candidate: Candidate) -> anyhow::Result<bool> {
            let id = candidate.instance.instance.0;
            self.attempts.lock().push(id);
            anyhow::ensure!(self.refused != Some(id), "candidate cannot be captured");
            let mut candidates = self.candidates.lock();
            let before = candidates.len();
            candidates.retain(|current| current.instance.instance != candidate.instance.instance);
            Ok(candidates.len() < before)
        }
    }

    fn candidate(id: u64, last_used_ms: u64, kind: ReclaimKind) -> Candidate {
        Candidate {
            cell: "cell".into(),
            instance: ReclaimCandidate {
                instance: isoplane_host::InstanceId(id),
                last_used_ms,
                kind,
            },
        }
    }

    fn coordinator(participants: Vec<TestParticipant>) -> Coordinator {
        Coordinator {
            participants: parking_lot::Mutex::new(
                participants
                    .into_iter()
                    .map(|participant| Arc::new(participant) as Arc<dyn Participant>)
                    .collect(),
            ),
            round: tokio::sync::Mutex::new(()),
            pressure: Arc::new(Pressure),
            policy: Default::default(),
            reclaiming: Default::default(),
        }
    }

    fn participant(
        candidates: Vec<Candidate>,
        attempts: &Arc<parking_lot::Mutex<Vec<u64>>>,
    ) -> TestParticipant {
        TestParticipant {
            candidates: parking_lot::Mutex::new(candidates),
            attempts: attempts.clone(),
            refused: None,
            unavailable: false,
        }
    }

    #[tokio::test]
    async fn all_cells_idle_candidates_precede_older_unfinished_work() {
        let attempts = Arc::new(parking_lot::Mutex::new(vec![]));
        let coordinator = coordinator(vec![
            participant(
                vec![
                    candidate(1, 1, ReclaimKind::Active),
                    candidate(2, 20, ReclaimKind::Idle),
                ],
                &attempts,
            ),
            participant(vec![candidate(3, 10, ReclaimKind::Idle)], &attempts),
        ]);
        coordinator
            .reclaim_memory()
            .await
            .expect("reclaim across participants");
        assert_eq!(
            *attempts.lock(),
            vec![3, 2, 1],
            "local LRU must drain completed work across cells before unfinished work"
        );
    }

    #[tokio::test]
    async fn an_oldest_capture_refusal_does_not_starve_later_eligible_candidates() {
        let attempts = Arc::new(parking_lot::Mutex::new(vec![]));
        let mut first = participant(
            vec![
                candidate(1, 1, ReclaimKind::Idle),
                candidate(2, 2, ReclaimKind::Idle),
            ],
            &attempts,
        );
        first.refused = Some(1);
        let coordinator = coordinator(vec![
            first,
            participant(vec![candidate(3, 0, ReclaimKind::Active)], &attempts),
        ]);
        coordinator
            .reclaim_memory()
            .await
            .expect("continue after capture refusal");
        assert_eq!(*attempts.lock(), vec![1, 2, 3], "a refused oldest candidate must be attempted once while later idle candidates retain priority");
    }

    #[tokio::test]
    async fn an_unavailable_participant_does_not_block_reclamation_in_healthy_participants() {
        let attempts = Arc::new(parking_lot::Mutex::new(vec![]));
        let mut unavailable = participant(vec![candidate(1, 1, ReclaimKind::Idle)], &attempts);
        unavailable.unavailable = true;
        let coordinator = coordinator(vec![
            unavailable,
            participant(vec![candidate(2, 2, ReclaimKind::Idle)], &attempts),
        ]);
        coordinator
            .reclaim_memory()
            .await
            .expect("continue with healthy participants");
        assert_eq!(
            *attempts.lock(),
            vec![2],
            "an unavailable participant cannot starve reachable eligible instances"
        );
    }

    #[tokio::test]
    async fn storage_reclamation_discards_only_the_globally_oldest_idle_snapshot() {
        let attempts = Arc::new(parking_lot::Mutex::new(vec![]));
        let coordinator = coordinator(vec![
            participant(
                vec![
                    candidate(1, 1, ReclaimKind::Active),
                    candidate(2, 20, ReclaimKind::HibernatedIdle),
                ],
                &attempts,
            ),
            participant(
                vec![candidate(3, 10, ReclaimKind::HibernatedIdle)],
                &attempts,
            ),
        ]);
        assert!(
            coordinator
                .reclaim_storage()
                .await
                .expect("reclaim snapshot capacity"),
            "an idle snapshot must release capacity for admission"
        );
        assert_eq!(
            *attempts.lock(),
            vec![3],
            "storage admission may discard only the globally oldest completed snapshot"
        );
    }


}
