use super::*;
use crate::cell::pool::cell_tests::{http, manifest, services};
use crate::fakes::{FakeActivator, FakeBehaviour, Step};
use crate::memory_pressure::{
    MemoryBytes, MemoryPressureError, MemoryPressureSource, MemorySample,
};
use isoplane_hibernate::snapshot::{
    SnapshotBytes, SnapshotError, SnapshotReservation, SnapshotStorage,
};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Default)]
struct StorageState {
    reservations: AtomicUsize,
    fail_save: AtomicBool,
}

struct Storage {
    state: Arc<StorageState>,
    limit: usize,
}

impl Storage {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::default(),
            limit,
        })
    }
}

impl SnapshotStorage for Storage {
    fn reserve(
        &self,
        capacity: SnapshotBytes,
    ) -> Result<Box<dyn SnapshotReservation>, SnapshotError> {
        self.state
            .reservations
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < self.limit).then_some(count + 1)
            })
            .map_err(|_| SnapshotError::Io {
                operation: "reserve",
                path: "test-storage".into(),
                source: std::io::ErrorKind::StorageFull.into(),
            })?;
        Ok(Box::new(Reservation {
            state: self.state.clone(),
            capacity,
            saved: None,
        }))
    }
}

struct Reservation {
    state: Arc<StorageState>,
    capacity: SnapshotBytes,
    saved: Option<Vec<u8>>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.state.reservations.fetch_sub(1, Ordering::Relaxed);
    }
}

impl SnapshotReservation for Reservation {
    fn reserved_bytes(&self) -> SnapshotBytes {
        self.capacity
    }
    fn save(&mut self, parts: &[std::io::IoSlice<'_>]) -> Result<(), SnapshotError> {
        if self.state.fail_save.load(Ordering::Relaxed) {
            return Err(SnapshotError::Io {
                operation: "save",
                path: "test-storage".into(),
                source: std::io::ErrorKind::StorageFull.into(),
            });
        }
        self.saved = Some(parts.iter().flat_map(|part| part.iter().copied()).collect());
        Ok(())
    }
    fn read_exact_at(
        &mut self,
        offset: u64,
        parts: &mut [std::io::IoSliceMut<'_>],
    ) -> Result<(), SnapshotError> {
        let saved = self.saved.as_ref().ok_or(SnapshotError::Empty)?;
        let mut at = offset as usize;
        for part in parts {
            let end = at + part.len();
            let bytes = saved.get(at..end).ok_or(SnapshotError::BeyondSnapshot {
                end: end as u64,
                saved: saved.len() as u64,
            })?;
            part.copy_from_slice(bytes);
            at = end;
        }
        Ok(())
    }
}

struct MemoryReads(Mutex<VecDeque<Option<MemorySample>>>);

impl MemoryPressureSource for MemoryReads {
    fn sample(&self) -> Result<MemorySample, MemoryPressureError> {
        self.0
            .lock()
            .pop_front()
            .expect("the sweep must not sample beyond the scripted recovery boundary")
            .ok_or(MemoryPressureError::Unsupported(
                "injected unavailable sample",
            ))
    }
}

fn sample(available: u64) -> MemorySample {
    MemorySample {
        available: MemoryBytes(available),
        total: MemoryBytes(100),
    }
}

fn test_cell(storage: Arc<Storage>, handler_after_ms: u64) -> Arc<Cell> {
    Cell::build(
        CellConfig {
            exec_threads: 0,
            hibernate: HibernateConfig {
                handler_after_ms,
                ..Default::default()
            },
            ..CellConfig::for_test("reclamation")
        },
        services(),
        storage,
    )
    .expect("the synchronous reclamation test cell must initialize")
}

fn register(
    cell: &Arc<Cell>,
    name: &str,
    lifecycle: HandlerLifecycle,
    behaviour: FakeBehaviour,
) -> ModuleId {
    let activator = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
    let module = activator.module().clone();
    let mut manifest = manifest(name);
    manifest.handlers.lifecycle = lifecycle;
    cell.register_module("acme", activator, manifest)
        .expect("reclamation fixture module must register");
    module
}

fn dispatch(cell: &Arc<Cell>, module: &ModuleId, id: u64) -> Receiver<DispatchResult> {
    let (responder, response) = std::sync::mpsc::sync_channel(1);
    let rec = http(id);
    cell.run_dispatch(DispatchJob {
        stream: None,
        upload: None,
        module: module.clone(),
        target: Target::Handler { route: "/".into() },
        completion: cell
            .completion_owner(module, &rec)
            .expect("registered completion owner"),
        rec,
        responder: responder.into(),
    });
    response
}

fn completed(cell: &Arc<Cell>, module: &ModuleId, id: u64) -> DispatchResult {
    dispatch(cell, module, id)
        .try_recv()
        .expect("a synchronous completed dispatch must answer")
}

fn idle(cell: &Arc<Cell>, name: &str, last_used_ms: u64) -> Arc<Slot> {
    let module = register(cell, name, HandlerLifecycle::Reuse, FakeBehaviour::done());
    let result = completed(cell, &module, 1);
    assert_eq!(
        result.status, 200,
        "the idle fixture request must complete successfully"
    );
    let slot = cell
        .slot(result.instance)
        .expect("completed handler resident");
    slot.park(last_used_ms);
    slot
}

fn active(
    cell: &Arc<Cell>,
    name: &str,
    last_used_ms: u64,
) -> (Arc<Slot>, Receiver<DispatchResult>) {
    let module = register(
        cell,
        name,
        HandlerLifecycle::Reuse,
        FakeBehaviour::program([Step::Park { imports: vec![] }]),
    );
    let response = dispatch(cell, &module, 1);
    assert!(
        matches!(response.try_recv(), Err(TryRecvError::Empty)),
        "a parked dispatch must retain its caller"
    );
    let slot = cell
        .residents
        .lock()
        .values()
        .find(|resident| resident.module == module)
        .expect("parked handler resident")
        .slot
        .clone();
    slot.park(last_used_ms);
    (slot, response)
}

fn candidate(cell: &Arc<Cell>, id: InstanceId) -> ReclaimCandidate {
    cell.reclamation_candidates(u64::MAX)
        .into_iter()
        .find(|candidate| candidate.instance == id)
        .expect("fixture instance must be eligible for reclamation")
}

#[test]
fn idle_handlers_precede_active_handlers_in_least_recently_used_order() {
    let cell = test_cell(Storage::new(8), 0);
    let (active_old, _old_response) = active(&cell, "active-old", 1);
    let idle_new = idle(&cell, "idle-new", 40);
    let (active_new, _new_response) = active(&cell, "active-new", 20);
    let idle_old = idle(&cell, "idle-old", 30);
    let order: Vec<_> = cell
        .reclamation_candidates(100)
        .into_iter()
        .map(|candidate| (candidate.instance, candidate.kind))
        .collect();
    assert_eq!(
        order,
        [
            (idle_old.id, ReclaimKind::Idle),
            (idle_new.id, ReclaimKind::Idle),
            (active_old.id, ReclaimKind::Active),
            (active_new.id, ReclaimKind::Active)
        ],
        "completed handlers must be reclaimed oldest first before any unfinished dispatch"
    );
    cell.shutdown();
}

#[test]
fn the_minimum_handler_age_is_checked_at_the_returned_stack_boundary() {
    let cell = test_cell(Storage::new(1), 10);
    let slot = idle(&cell, "aged", 100);
    assert!(
        cell.reclamation_candidates(109).is_empty(),
        "a handler below the minimum idle age must remain ineligible"
    );
    assert_eq!(
        cell.reclamation_candidates(110)[0].instance,
        slot.id,
        "a handler becomes eligible exactly at its configured age"
    );
}

#[test]
fn activity_in_the_millisecond_a_candidate_was_listed_still_invalidates_it() {
    let cell = test_cell(Storage::new(1), 0);
    let slot = idle(&cell, "same-millisecond", 10);
    let stale = candidate(&cell, slot.id);
    slot.park(10);
    assert!(
        !cell
            .reclaim_candidate(stale)
            .expect("stale candidate refusal"),
        "activity must invalidate a candidate even when the clock reads the same"
    );
    assert_eq!(slot.state(), LifecycleTier::Parked);
}

#[test]
fn a_candidate_observed_before_recent_activity_cannot_reclaim_the_slot() {
    let cell = test_cell(Storage::new(1), 0);
    let slot = idle(&cell, "recent", 10);
    let stale = candidate(&cell, slot.id);
    slot.park(20);
    assert!(
        !cell
            .reclaim_candidate(stale)
            .expect("stale candidate refusal"),
        "new activity must invalidate a previously observed LRU candidate"
    );
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "a stale candidate must leave resident memory available"
    );
    assert!(
        cell.reclaim_candidate(candidate(&cell, slot.id))
            .expect("current candidate capture"),
        "the current candidate must still be reclaimable"
    );
}

#[test]
fn an_idle_reuse_handler_restores_its_snapshot_through_pool_acquisition() {
    let cell = test_cell(Storage::new(1), 0);
    let module = register(
        &cell,
        "reuse",
        HandlerLifecycle::Reuse,
        FakeBehaviour::done().with_lazy_init_probe(),
    );
    let first = completed(&cell, &module, 1);
    assert!(
        cell.reclaim_candidate(candidate(&cell, first.instance))
            .expect("idle reuse capture"),
        "the completed reuse handler must hibernate"
    );
    let second = completed(&cell, &module, 2);
    assert_eq!(
        second.acquired, "hibernation_wake",
        "pool acquisition must restore its hibernated warm entry"
    );
    assert_eq!(
        second.instance, first.instance,
        "reuse restoration must retain the pooled identity"
    );
    assert_ne!(
        second.body, first.body,
        "reuse restoration must preserve the guest's lazy initialization residue"
    );
}

#[test]
fn a_hibernated_fresh_candidate_is_reset_before_the_next_request() {
    let cell = test_cell(Storage::new(1), 0);
    let module = register(
        &cell,
        "fresh",
        HandlerLifecycle::Fresh,
        FakeBehaviour::done().with_lazy_init_probe(),
    );
    let first = completed(&cell, &module, 1);
    assert!(
        cell.reclaim_candidate(candidate(&cell, first.instance))
            .expect("fresh candidate capture"),
        "the completed fresh candidate must hibernate before reset"
    );
    let second = completed(&cell, &module, 2);
    assert_eq!(
        second.acquired, "pooled_fresh",
        "fresh pool acquisition must perform its deferred reset after restoration"
    );
    assert_eq!(
        second.instance, first.instance,
        "reset must reuse the hibernated candidate's slot"
    );
    assert_eq!(
        second.body, first.body,
        "fresh acquisition must clear prior request initialization residue"
    );
}

#[test]
fn memory_recovery_stops_the_sweep_before_an_active_dispatch_is_captured() {
    let cell = test_cell(Storage::new(2), 0);
    let (active, _response) = active(&cell, "active", 1);
    let idle = idle(&cell, "idle", 2);
    *cell.memory_pressure.write() = Arc::new(MemoryReads(Mutex::new(
        [Some(sample(5)), Some(sample(15))].into(),
    )));
    cell.sweep_handler_pressure();
    assert_eq!(
        idle.state(),
        LifecycleTier::Hibernated,
        "pressure must reclaim the completed handler first"
    );
    assert_eq!(
        active.state(),
        LifecycleTier::Parked,
        "recovery must stop reclamation before touching an unfinished request"
    );
    assert!(
        !cell.reclaiming.load(Ordering::Relaxed),
        "the recovery sample must clear pressure hysteresis"
    );
    cell.shutdown();
}

#[test]
fn an_unavailable_memory_sample_leaves_resident_memory_owned() {
    let cell = test_cell(Storage::new(1), 0);
    let slot = idle(&cell, "unmeasured", 1);
    *cell.memory_pressure.write() = Arc::new(MemoryReads(Mutex::new([None].into())));
    cell.sweep_handler_pressure();
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "sampling failure must not invent memory pressure"
    );
    assert_eq!(
        cell.metrics().captures,
        0,
        "an unavailable sample must not count a capture"
    );
}

#[test]
fn a_snapshot_write_failure_keeps_the_source_guest_resident() {
    let storage = Storage::new(1);
    let cell = test_cell(storage.clone(), 0);
    let slot = idle(&cell, "write-failure", 1);
    storage.state.fail_save.store(true, Ordering::Relaxed);
    assert!(
        cell.reclaim_candidate(candidate(&cell, slot.id)).is_err(),
        "a storage write failure must refuse capture"
    );
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "failed persistence must leave the only guest copy resident"
    );
    assert_eq!(
        cell.metrics().capture_write_failures,
        1,
        "capture write failure must remain observable"
    );
    assert_eq!(
        storage.state.reservations.load(Ordering::Relaxed),
        1,
        "a failed capture must retain its admitted storage reservation"
    );
}

#[test]
fn storage_pressure_evicts_an_idle_snapshot_before_admitting_another_handler() {
    let journal = TransitionJournal::default();
    let _journal = journal.install();
    let storage = Storage::new(1);
    let cell = test_cell(storage.clone(), 0);
    let cached = idle(&cell, "cached", 1);
    assert!(
        cell.reclaim_candidate(candidate(&cell, cached.id))
            .expect("cached capture"),
        "the completed handler must have an evictable snapshot"
    );
    let module = register(&cell, "new", HandlerLifecycle::Reuse, FakeBehaviour::done());
    let result = completed(&cell, &module, 2);
    assert_eq!(
        result.status, 200,
        "releasing a cached idle snapshot must permit a new reservation"
    );
    assert_eq!(
        cached.state(),
        LifecycleTier::Discarded,
        "capacity reclamation must evict the idle cached identity"
    );
    assert!(
        cell.slot(cached.id).is_none(),
        "evicted idle snapshots must leave the resident registry"
    );
    assert_eq!(
        storage.state.reservations.load(Ordering::Relaxed),
        1,
        "the new handler must own the reclaimed capacity"
    );
    let evictions = journal.evictions();
    assert_eq!(
        evictions.len(),
        1,
        "one successful idle eviction must emit one transition"
    );
    assert_eq!(
        evictions[0]["target"], "isoplane_host::cell::transition",
        "evictions must use the exported lifecycle target"
    );
    assert_eq!(
        evictions[0]["fields"],
        serde_json::json!({
            "event": "instance_transition", "tenant": "acme", "app": "cached", "version": "v1",
            "instance": cached.id.0, "previous": "hibernated", "current": "discarded", "cause": "storage_pressure",
        }),
        "eviction must report the established owner, actual resulting tier and cause"
    );
}

#[test]
fn storage_pressure_cannot_evict_an_active_dispatch_snapshot() {
    let journal = TransitionJournal::default();
    let _journal = journal.install();
    let storage = Storage::new(1);
    let cell = test_cell(storage.clone(), 0);
    let (active, response) = active(&cell, "protected", 1);
    assert!(
        cell.reclaim_candidate(candidate(&cell, active.id))
            .expect("active capture"),
        "the parked active request must be locally suspendable"
    );
    let module = register(&cell, "new", HandlerLifecycle::Reuse, FakeBehaviour::done());
    let refused = completed(&cell, &module, 2);
    assert_eq!(
        refused.status, 503,
        "exhausted reservations must refuse new activation rather than steal an active snapshot"
    );
    assert_eq!(
        refused.body, b"snapshot_capacity_unavailable",
        "capacity refusal must identify the retryable admission failure"
    );
    assert_eq!(
        active.state(),
        LifecycleTier::Hibernated,
        "storage pressure must retain the unfinished request's only continuation"
    );
    assert!(
        cell.active.contains_key(&active.id),
        "storage pressure must preserve active ownership"
    );
    assert!(
        journal.evictions().is_empty(),
        "refusing to evict an active snapshot must not emit a discarded transition"
    );
    assert!(
        matches!(response.try_recv(), Err(TryRecvError::Empty)),
        "a refused competing admission must not end the protected caller"
    );
    assert_eq!(
        storage.state.reservations.load(Ordering::Relaxed),
        1,
        "the active continuation keeps its reserved capacity"
    );
    cell.shutdown();
}

#[test]
fn memory_pressure_hysteresis_uses_distinct_trigger_and_recovery_boundaries() {
    let cfg = HibernateConfig {
        reclaim_below_percent: 10,
        recover_above_percent: 20,
        ..Default::default()
    };
    assert!(
        cfg.pressure_requested(sample(9), false),
        "available memory below the trigger must start reclamation"
    );
    assert!(
        !cfg.pressure_requested(sample(10), false),
        "memory exactly at the trigger must not start reclamation"
    );
    assert!(
        cfg.pressure_requested(sample(19), true),
        "reclamation must persist below the recovery threshold"
    );
    assert!(
        !cfg.pressure_requested(sample(20), true),
        "memory exactly at recovery must stop reclamation"
    );
}

#[test]
fn zero_effective_capacity_requires_reclamation() {
    let cfg = HibernateConfig::default();
    assert!(
        cfg.pressure_requested(
            MemorySample {
                available: MemoryBytes(0),
                total: MemoryBytes(0)
            },
            false
        ),
        "zero effective capacity must remain under pressure"
    );
}

#[test]
fn large_memory_samples_do_not_overflow_pressure_accounting() {
    let cfg = HibernateConfig::default();
    assert!(
        !cfg.pressure_requested(
            MemorySample {
                available: MemoryBytes(u64::MAX),
                total: MemoryBytes(u64::MAX)
            },
            false
        ),
        "large healthy hosts must not overflow into memory pressure"
    );
}

#[test]
fn invalid_pressure_thresholds_are_refused_before_startup() {
    for (trigger, recovery) in [(10, 10), (20, 10), (0, 101)] {
        let cfg = HibernateConfig {
            reclaim_below_percent: trigger,
            recover_above_percent: recovery,
            ..Default::default()
        };
        assert!(
            cfg.validate().is_err(),
            "thresholds must require increasing hysteresis within a percentage"
        );
    }
}

#[test]
fn healthy_memory_does_not_capture_an_old_idle_handler() {
    let cell = test_cell(Storage::new(1), 0);
    let slot = idle(&cell, "healthy", 1);
    *cell.memory_pressure.write() = Arc::new(MemoryReads(Mutex::new([Some(sample(50))].into())));
    cell.sweep_handler_pressure();
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "age eligibility alone must not reclaim handlers without memory pressure"
    );
}

#[test]
fn a_node_coordinator_exclusively_owns_handler_pressure_sampling() {
    let cell = test_cell(Storage::new(1), 0);
    let slot = idle(&cell, "coordinated", 1);
    *cell.memory_pressure.write() = Arc::new(MemoryReads(Mutex::new(VecDeque::new())));
    cell.set_node_coordinated_reclamation();
    cell.sweep_handler_pressure();
    assert_eq!(
        slot.state(),
        LifecycleTier::Parked,
        "a coordinated cell must leave handler pressure decisions to its node owner"
    );
}

#[derive(Clone, Default)]
struct TransitionJournal(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for TransitionJournal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("journal lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TransitionJournal {
    fn install(&self) -> tracing::dispatcher::DefaultGuard {
        let journal = self.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(move || journal.clone())
            .finish();
        tracing::dispatcher::set_default(&tracing::Dispatch::new(subscriber))
    }

    fn evictions(&self) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .expect("journal lock")
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_slice::<serde_json::Value>(line).expect("structured journal")
            })
            .filter(|event| {
                event["fields"]["event"] == "instance_transition"
                    && event["fields"]["cause"] == "storage_pressure"
            })
            .collect()
    }
}
