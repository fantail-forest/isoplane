//! Saves local snapshots while the instance retains its original host context.

use std::sync::Arc;

use isoplane_engine::Completion;

use crate::pool::Activator;
use crate::slot::Slot;

/// The GC-hint pressure value that means "about to snapshot".
pub const GC_HINT_SNAPSHOT: u8 = 255;

/// Every one of these is a refusal: hibernation is an efficiency mechanism,
/// never a correctness requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureRefused {
    NotParked,
    /// An HTTP response still owns a live requester transport.
    OpenResponse,
    /// An unread or in-flight client upload cannot be preserved in a guest checkpoint.
    OpenRequest,
    /// An outbound HTTP operation or response body still owns a transport.
    OutboundHttp,
    /// The pre-capture gc hint did not return `done`. `Parked` means the
    /// guest awaited inside the hint and stays resident; any other
    /// completion means the instance has been discarded.
    GcHintDidNotComplete(Completion),
    /// The hint completed but the instance is not at a serializable point.
    StackNotEmptyAfterHint,
    /// The live instance retains state that the local snapshot format cannot preserve.
    UnrepresentableState(isoplane_engine::snapshot::SnapshotError),
    /// Capture read the instance's memory or its template's baseline from
    /// storage that had failed, so the snapshot is invalid. The instance has been
    /// destroyed.
    StorageFailed,
    /// Writing the snapshot to the instance's reservation failed. The
    /// instance stays resident.
    SnapshotWrite(String),
    Failed(String),
}


/// Observes guest work invoked by capture on behalf of an unfinished dispatch.
pub trait CaptureObserver: Send + Sync {
    /// Marks the start of a hint; no end callback means its measurement is incomplete.
    fn begin_hint(&self);
    /// Records a returned hint's measured usage, including a trapping return.
    fn end_hint(&self, outcome: &isoplane_engine::DispatchOutcome);
}

/// What a capture needs besides the slot.
pub struct CaptureEnv<'a> {
    /// Whether the execution owner still holds an unfinished dispatch.
    pub dispatch_active: bool,
    /// The active dispatch's diagnostic usage owner, when attached by the cell.
    pub observer: Option<Arc<dyn CaptureObserver>>,
    pub activator: &'a Arc<dyn Activator>,
}



/// What an in-process capture saved.
pub struct LocalCapture {
    pub saved: isoplane_hibernate::local::Saved,
    pub capture_us: u64,
    pub gc_hint_us: u64,
}

/// Captures into the slot's own reservation while its original context keeps
/// every live future, so the snapshot is usable only with that context.
pub(crate) fn capture_in_context(
    slot: &Arc<Slot>,
    env: CaptureEnv<'_>,
) -> Result<LocalCapture, CaptureRefused> {
    let template = env.activator.template_image();
    let binding =
        isoplane_hibernate::Binding::new(env.activator.module_hash(), env.activator.abi_rev());
    let (captured, gc_hint_us) = capture_with(slot, &env, &template, |instance, image| {
        let started = std::time::Instant::now();
        let changed = instance.changed_ranges();
        let mut reservation = slot.ctx.snapshot.lock();
        let reservation = reservation
            .as_mut()
            .ok_or_else(|| CaptureRefused::Failed("instance has no snapshot reservation".into()))?;
        let saved = isoplane_hibernate::local::save(
            reservation.as_mut(),
            binding,
            image,
            instance.memory(),
            &template,
            changed.as_deref(),
        )
        .map_err(|e| CaptureRefused::SnapshotWrite(format!("{e:#}")))?;
        Ok((saved, started.elapsed().as_micros() as u64))
    })?;
    Ok(LocalCapture {
        saved: captured.0,
        capture_us: captured.1,
        gc_hint_us,
    })
}

/// Everything before the memory is read: eligibility, the gc hint for an
/// active dispatch, and globals and tables. `finish` reads the memory; a
/// capture that read failed storage destroys the instance.
fn capture_with<T>(
    slot: &Arc<Slot>,
    env: &CaptureEnv<'_>,
    template: &isoplane_engine::MemoryImage,
    finish: impl FnOnce(
        &mut (dyn isoplane_engine::InstanceOps + Send),
        isoplane_engine::InstanceImage,
    ) -> Result<T, CaptureRefused>,
) -> Result<(T, u64), CaptureRefused> {
    if slot.ctx.has_active_request_body() {
        return Err(CaptureRefused::OpenRequest);
    }
    if slot.ctx.has_open_transport() {
        return Err(CaptureRefused::OpenResponse);
    }
    if !slot.is_capture_eligible() {
        return Err(CaptureRefused::NotParked);
    }

    let mut guard = slot.enter().map_err(|_| CaptureRefused::NotParked)?;
    if !guard.instance().is_capture_eligible() {
        return Err(CaptureRefused::NotParked);
    }

    let gc_hint_us = if env.dispatch_active {
        let t_gc = std::time::Instant::now();
        if let Some(observer) = &env.observer {
            observer.begin_hint();
        }
        let hint = tracing::info_span!("capture_hint")
            .in_scope(|| guard.instance().gc_hint(GC_HINT_SNAPSHOT));
        if let Some(observer) = &env.observer {
            observer.end_hint(&hint);
        }
        let elapsed_us = t_gc.elapsed().as_micros() as u64;
        if hint.completion != Completion::Done {
            // A trap or kill inside the hint has discarded the engine's
            // instance; the slot follows, so the sweep stops selecting it and
            // `Slot::enter` refuses. A guest that parked inside the hint is
            // consistent and stays as it is.
            if !hint.completion.instance_survives() {
                guard.destroy();
            }
            return Err(CaptureRefused::GcHintDidNotComplete(hint.completion));
        }
        elapsed_us
    } else {
        // A completed guest may be captured, but cannot execute maintenance callbacks.
        0
    };
    // Eligibility rather than parked: an instance that returned `done` and
    // went idle has an empty stack too.
    if !guard.instance().is_capture_eligible() {
        return Err(CaptureRefused::StackNotEmptyAfterHint);
    }

    if slot.ctx.has_active_request_body() {
        return Err(CaptureRefused::OpenRequest);
    }
    if slot.ctx.has_outbound_http() {
        return Err(CaptureRefused::OutboundHttp);
    }

    let image = guard.instance().capture_mutable_state().map_err(|error| {
        match error.downcast::<isoplane_engine::snapshot::SnapshotError>() {
            Ok(error) => CaptureRefused::UnrepresentableState(error),
            Err(error) => CaptureRefused::Failed(error.to_string()),
        }
    })?;

    let finished = finish(guard.instance(), image)?;
    if guard.instance().storage_failed() || template.storage_failed() {
        guard.destroy();
        return Err(CaptureRefused::StorageFailed);
    }
    Ok((finished, gc_hint_us))
}



#[cfg(test)]
mod tests {
    use crate::futures::FutureTable;
    use super::*;
    use crate::pool::InstancePool;

    /// A guest whose pre-capture gc hint ends however the test says and which,
    /// like the engine, answers every later call with a trap once discarded.
    struct HintEndsIn {
        how: Completion,
        discarded: bool,
        memory: Vec<u8>,
        gc_upload: Option<Arc<crate::bindings::request_body::RequestBodyScope>>,
        pending_upload: Option<crate::bindings::request_body::RequestRead>,
        storage_failed: bool,
    }

    impl HintEndsIn {
        fn outcome(&self, completion: Completion) -> isoplane_engine::DispatchOutcome {
            isoplane_engine::DispatchOutcome {
                status: match completion {
                    Completion::Parked => isoplane_abi::status::PollStatus::Pending,
                    Completion::Done => isoplane_abi::status::PollStatus::Done { response: 0 },
                    _ => isoplane_abi::status::PollStatus::Trapped {
                        code: isoplane_abi::status::TrapCode::Panic,
                    },
                },
                completion,
                occupancy_ns: 0,
                occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
                execution_limit: None,
                wall_ns: 0,
                trap: None,
                grace_end: None,
                overage_ns: 0,
                grace_deferrals: 0,
            }
        }
    }

    impl isoplane_engine::InstanceOps for HintEndsIn {
        fn dispatch(
            &mut self,
            _rec: &isoplane_abi::dispatch::DispatchRecord,
            _b: isoplane_engine::OccupancyBudget,
        ) -> isoplane_engine::DispatchOutcome {
            if self.discarded {
                return self.outcome(Completion::Trapped);
            }
            self.outcome(Completion::Done)
        }
        fn wake(
            &mut self,
            _d: u64,
            _f: u64,
            _r: &isoplane_abi::result::ResultRecord,
            _b: isoplane_engine::OccupancyBudget,
        ) -> isoplane_engine::DispatchOutcome {
            if self.discarded {
                return self.outcome(Completion::Trapped);
            }
            self.outcome(Completion::Done)
        }
        fn gc_hint(&mut self, _pressure: u8) -> isoplane_engine::DispatchOutcome {
            if let Some(upload) = &self.gc_upload {
                self.pending_upload =
                    Some(upload.begin_read().expect("GC probe starts its EOF read"));
            }
            if self.discarded {
                return self.outcome(Completion::Trapped);
            }
            if !self.how.instance_survives() {
                self.discarded = true;
            }
            self.outcome(self.how)
        }
        fn is_parked(&self) -> bool {
            !self.discarded
        }
        fn is_capture_eligible(&self) -> bool {
            !self.discarded
        }
        fn memory(&mut self) -> &mut [u8] {
            &mut self.memory
        }
        fn memory_len(&self) -> usize {
            self.memory.len()
        }
        fn storage_failed(&self) -> bool {
            self.storage_failed
        }
        fn capture_mutable_state(&mut self) -> anyhow::Result<isoplane_engine::InstanceImage> {
            anyhow::ensure!(!self.discarded, "discarded");
            Ok(Default::default())
        }
        fn restore_mutable_state(
            &mut self,
            _img: &isoplane_engine::InstanceImage,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct HintActivator {
        how: Completion,
        module: crate::ModuleId,
        template: Arc<Vec<u8>>,
        upload_capable: bool,
    }

    impl HintActivator {
        fn new(how: Completion) -> Self {
            Self {
                how,
                module: crate::ModuleId::new("hint", "v1"),
                template: Arc::new(vec![0u8; isoplane_abi::PAGE_SIZE]),
                upload_capable: false,
            }
        }
    }

    impl Activator for HintActivator {
        fn imports(&self, module: &str, name: &str) -> bool {
            self.upload_capable
                && module == isoplane_abi::imports::HOST_MODULE
                && name == "req_body_read"
        }
        fn module(&self) -> &crate::ModuleId {
            &self.module
        }
        fn module_hash(&self) -> isoplane_engine::ModuleHash {
            isoplane_engine::ModuleHash::of(&self.template)
        }
        fn abi_rev(&self) -> u32 {
            isoplane_abi::ABI_REV
        }
        fn template_image(&self) -> Arc<isoplane_engine::MemoryImage> {
            Arc::new(isoplane_engine::MemoryImage::from_dense(&self.template))
        }
        fn snapshot_size_bound(&self, _max_memory_bytes: u64) -> anyhow::Result<u64> {
            isoplane_hibernate::local::size_bound(isoplane_hibernate::local::SnapshotShape {
                max_memory_bytes: self.template.len() as u64,
                mutable_globals: 0,
            })
        }
        fn activate(
            &self,
            _b: Box<dyn isoplane_engine::HostBindings>,
        ) -> anyhow::Result<Box<dyn isoplane_engine::InstanceOps + Send>> {
            Ok(Box::new(HintEndsIn {
                how: self.how,
                discarded: false,
                memory: (*self.template).clone(),
                gc_upload: None,
                pending_upload: None,
                storage_failed: false,
            }))
        }
    }

    fn parked_slot(how: Completion) -> (Arc<Slot>, Arc<dyn Activator>) {
        use isoplane_abi::config::Limits;
        use isoplane_hibernate::snapshot::SnapshotStorage;
        let act: Arc<dyn Activator> = Arc::new(HintActivator::new(how));
        let pool = InstancePool::new(act.clone(), 4);
        let (slot, _) = pool
            .acquire(|id| {
                Arc::new(crate::bindings::InstanceCtx::for_test(
                    id,
                    Limits::default(),
                ))
            })
            .expect("a slot");
        let storage = tempfile::tempdir().expect("snapshot directory");
        let reservation = isoplane_hibernate::snapshot::LocalSnapshotStorage::new(storage.path())
            .expect("snapshot storage")
            .reserve(isoplane_hibernate::snapshot::SnapshotBytes(
                act.snapshot_size_bound(isoplane_abi::PAGE_SIZE as u64)
                    .expect("bound the fixture's snapshot"),
            ))
            .expect("reserve the fixture's snapshot");
        *slot.ctx.snapshot.lock() = Some(reservation);
        slot.park(0);
        assert!(
            slot.is_capture_eligible(),
            "the fixture must start capturable"
        );
        (slot, act)
    }

    fn try_capture(
        slot: &Arc<Slot>,
        act: &Arc<dyn Activator>,
    ) -> Result<LocalCapture, CaptureRefused> {
        capture_in_context(
            slot,
            CaptureEnv {
                observer: None,
                dispatch_active: true,
                activator: act,
            },
        )
    }

    #[test]
    fn in_process_capture_preserves_pending_host_work() {
        use isoplane_abi::result::ResultRecord;
        let (slot, act) = parked_slot(Completion::Done);
        let futures = Arc::new(FutureTable::new());
        let id = futures
            .alloc(slot.id, 1, "http_body_read", 8)
            .expect("the request must own a pending future");
        capture_in_context(
            &slot,
            CaptureEnv {
                observer: None,
                dispatch_active: true,
                activator: &act,
            },
        )
        .expect("capture must retain the original host context");
        assert!(
            futures.take_ready(slot.id).is_none(),
            "local capture must keep pending I/O pending"
        );
        futures.complete(id, ResultRecord::Ok(b"completed while asleep".to_vec()));
        assert!(
            matches!(futures.take_ready(slot.id).map(crate::futures::Ready::into_pair), Some((ready, ResultRecord::Ok(bytes))) if ready == id && bytes == b"completed while asleep"),
            "the original operation must deliver its result after local capture"
        );
    }

    #[test]
    fn in_process_capture_retains_ready_results() {
        use isoplane_abi::result::ResultRecord;
        let (slot, act) = parked_slot(Completion::Done);
        let futures = Arc::new(FutureTable::new());
        let id = futures
            .alloc(slot.id, 1, "clock_deadline_in", 8)
            .expect("the request must own its result");
        futures.complete(id, ResultRecord::Ok(vec![42; 32]));
        capture_in_context(
            &slot,
            CaptureEnv {
                observer: None,
                dispatch_active: true,
                activator: &act,
            },
        )
        .expect("a host-resident result must not prevent local capture");
        assert!(
            matches!(futures.take_ready(slot.id).map(crate::futures::Ready::into_pair), Some((ready, ResultRecord::Ok(bytes))) if ready == id && bytes == vec![42; 32]),
            "local capture must preserve the exact ready result"
        );
    }

    /// The engine's discard and the slot's must agree, or the idle sweep
    /// re-selects a Parked slot every tick and re-enters a killed instance.
    #[test]
    fn a_guest_that_traps_in_its_gc_hint_is_discarded_on_the_slot_too() {
        for how in [Completion::Trapped, Completion::BudgetKill] {
            let (slot, act) = parked_slot(how);

            let refused = try_capture(&slot, &act)
                .err()
                .expect("a trap is not a capture");
            assert_eq!(
                refused,
                CaptureRefused::GcHintDidNotComplete(how),
                "{how:?}: the refusal has to say how the hint ended, because that is what \
                 decides whether the instance may ever be entered again"
            );

            assert!(
                matches!(
                    slot.enter(),
                    Err(crate::slot::EnterError::NotResident(
                        crate::slot::LifecycleTier::Discarded
                    ))
                ),
                "{how:?}: the engine discarded itself, so the slot's front door must refuse"
            );
            assert_ne!(
                slot.state(),
                crate::slot::LifecycleTier::Parked,
                "{how:?}: still Parked means the idle sweep will select it again next tick"
            );

            assert_eq!(
                try_capture(&slot, &act).err(),
                Some(CaptureRefused::NotParked),
                "{how:?}: a second capture attempt must be refused at the door, not run"
            );
        }
    }

    #[test]
    fn a_capture_that_read_failed_storage_is_refused_and_destroys_the_instance() {
        let (initial, act) = parked_slot(Completion::Done);
        let slot = Arc::new(Slot::new(
            initial.id,
            initial.ctx.clone(),
            Box::new(HintEndsIn {
                how: Completion::Done,
                discarded: false,
                memory: vec![0; isoplane_abi::PAGE_SIZE],
                gc_upload: None,
                pending_upload: None,
                storage_failed: true,
            }),
        ));
        slot.park(0);
        assert_eq!(
            try_capture(&slot, &act).err(),
            Some(CaptureRefused::StorageFailed),
            "a snapshot read from failed storage must never be kept"
        );
        assert_eq!(
            slot.state(),
            crate::slot::LifecycleTier::Discarded,
            "an instance whose memory read failed storage must not run again"
        );
    }

    #[test]
    fn a_guest_that_parks_inside_its_gc_hint_stays_resident() {
        let (slot, act) = parked_slot(Completion::Parked);
        assert_eq!(
            try_capture(&slot, &act).err(),
            Some(CaptureRefused::GcHintDidNotComplete(Completion::Parked)),
            "parking inside the hint refuses the capture"
        );
        assert_eq!(
            slot.state(),
            crate::slot::LifecycleTier::Parked,
            "parking in the hint is not a fault"
        );
        assert!(
            slot.is_capture_eligible(),
            "and the next sweep may try again"
        );
        assert!(slot.enter().is_ok(), "the instance is still callable");
    }

    #[tokio::test]
    async fn upload_eof_ready_before_delivery_survives_capture_and_repeated_reads() {
        use crate::bindings::request_body::RequestBodyScope;
        use isoplane_abi::result::ResultRecord;
        use std::num::NonZeroU32;
        let (slot, act) = parked_slot(Completion::Done);
        let source = RequestBodyScope::new(None, true);
        *slot.ctx.start_dispatch(1, 1).request_body.lock() = Some(source.clone());
        let futures = Arc::new(FutureTable::new());
        let id = futures
            .alloc(slot.id, 1, "req_body_read", 8)
            .expect("allocate EOF read");
        let eof = source
            .begin_read()
            .expect("open upload")
            .read(NonZeroU32::new(1).expect("positive read"))
            .await
            .expect("EOF succeeds");
        assert!(
            eof.is_empty(),
            "the source must finish before its result is delivered"
        );
        futures.complete(id, ResultRecord::Ok(eof));
        capture_in_context(
            &slot,
            CaptureEnv {
                observer: None,
                dispatch_active: true,
                activator: &act,
            },
        )
        .expect("EOF with a ready result permits local capture");
        assert!(
            matches!(futures.take_ready(slot.id).map(crate::futures::Ready::into_pair), Some((ready_id, ResultRecord::Ok(bytes))) if ready_id == id && bytes.is_empty()),
            "the original context must receive the original EOF result"
        );
        assert_eq!(
            source.can_have_body(),
            Ok(true),
            "HTTP framing metadata remains available after local capture"
        );
        assert!(
            source
                .begin_read()
                .expect("EOF source remains open")
                .read(NonZeroU32::new(1).expect("positive read"))
                .await
                .expect("repeated EOF succeeds")
                .is_empty(),
            "later reads remain EOF rather than a capability error"
        );
    }

    #[test]
    fn an_upload_read_started_by_the_gc_hint_vetoes_capture() {
        use crate::bindings::request_body::RequestBodyScope;
        let (initial, act) = parked_slot(Completion::Done);
        let source = RequestBodyScope::new(None, true);
        *initial.ctx.start_dispatch(1, 1).request_body.lock() = Some(source.clone());
        let slot = Arc::new(Slot::new(
            initial.id,
            initial.ctx.clone(),
            Box::new(HintEndsIn {
                how: Completion::Done,
                discarded: false,
                memory: vec![0; isoplane_abi::PAGE_SIZE],
                gc_upload: Some(source),
                pending_upload: None,
                storage_failed: false,
            }),
        ));
        slot.park(0);
        assert_eq!(
            try_capture(&slot, &act).err(),
            Some(CaptureRefused::OpenRequest),
            "capture must recheck upload work started by the GC hint under execution exclusion"
        );
    }
}
