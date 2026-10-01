//! Captures same-process sandbox state while its original host context owns live resources.

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
    /// guest awaited inside the hint and stays resident; a completion that
    /// tombstones means the instance has been destroyed.
    GcHintDidNotComplete(Completion),
    /// The hint completed but the instance is not at a serializable point.
    StackNotEmptyAfterHint,
    /// The live instance retains state that the current delta format cannot preserve.
    UnrepresentableState(isoplane_engine::snapshot::SnapshotError),
    /// Capture read the instance's memory or its template's baseline from
    /// storage that had failed, so the delta is wrong. The instance has been
    /// destroyed.
    StorageFailed,
    Failed(String),
    /// Local publication failed; the resident instance remains owned.
    SnapshotWrite(String),
}

/// Published local state and measured capture work.
pub(crate) struct LocalCapture {
    /// Bytes fully published to the local owner.
    pub bytes: u64,
    /// Pages that differ from the template.
    pub changed_pages: u32,
    /// Local encoding and publication elapsed microseconds.
    pub capture_us: u64,
    /// Guest GC hint elapsed microseconds.
    pub gc_hint_us: u64,
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

/// Captures sandbox state into storage owned by the original host context.
pub(crate) fn capture_in_context(
    slot: &Arc<Slot>,
    env: CaptureEnv<'_>,
) -> Result<LocalCapture, CaptureRefused> {
    let template = env.activator.template_image();
    let (captured, gc_hint_us) = capture_with(slot, &env, &template, |instance, image| {
        let started = std::time::Instant::now();
        let identity = local_snapshot::Identity {
            module: env.activator.module_hash(),
            abi_rev: env.activator.abi_rev(),
            instance: slot.id,
        };
        let mut reservation = slot.ctx.snapshot.lock();
        let reservation = reservation
            .as_mut()
            .ok_or_else(|| CaptureRefused::Failed("instance has no snapshot reservation".into()))?;
        let storage_bytes = reservation.reserved_bytes().0;
        let encoded = local_snapshot::encode(
            identity,
            local_snapshot::Bounds {
                memory_bytes: slot.ctx.limits.instance_memory_max,
                storage_bytes,
            },
            image,
            instance.memory(),
            &template,
        )
        .map_err(|error| CaptureRefused::Failed(error.to_string()))?;
        let bytes = encoded.bytes.len() as u64;
        reservation
            .save(&encoded.bytes)
            .map_err(|error| CaptureRefused::SnapshotWrite(error.to_string()))?;
        Ok((
            bytes,
            encoded.changed_pages,
            started.elapsed().as_micros() as u64,
        ))
    })?;
    Ok(LocalCapture {
        bytes: captured.0,
        changed_pages: captured.1,
        capture_us: captured.2,
        gc_hint_us,
    })
}

fn capture_with<T>(
    slot: &Arc<Slot>,
    env: &CaptureEnv<'_>,
    template: &isoplane_engine::MemoryImage,
    finish: impl FnOnce(
        &mut (dyn isoplane_engine::InstanceOps + Send),
        isoplane_engine::InstanceImage,
    ) -> Result<T, CaptureRefused>,
) -> Result<(T, u64), CaptureRefused> {
    if slot
        .ctx
        .request_body
        .lock()
        .as_ref()
        .is_some_and(|body| body.is_active())
    {
        return Err(CaptureRefused::OpenRequest);
    }
    if slot.ctx.responses.lock().has_open_transport() {
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
            // A trap or hard kill inside the hint has tombstoned the engine's
            // instance; the slot follows, so the sweep stops selecting it and
            // `Slot::enter` refuses. A guest that parked inside the hint is
            // consistent and stays as it is.
            if hint.completion.tombstones() {
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

    if slot
        .ctx
        .request_body
        .lock()
        .as_ref()
        .is_some_and(|body| body.is_active())
    {
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
    use super::*;
    use crate::pool::InstancePool;

    /// A guest whose pre-capture gc hint ends however the test says and which,
    /// like the engine, answers every later call with a trap once tombstoned.
    struct HintEndsIn {
        how: Completion,
        tombstoned: bool,
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
                kill: None,
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
            if self.tombstoned {
                return self.outcome(Completion::Trapped);
            }
            self.outcome(Completion::Done)
        }
        fn wake(
            &mut self,
            _f: u64,
            _r: &isoplane_abi::result::ResultRecord,
            _b: isoplane_engine::OccupancyBudget,
        ) -> isoplane_engine::DispatchOutcome {
            if self.tombstoned {
                return self.outcome(Completion::Trapped);
            }
            self.outcome(Completion::Done)
        }
        fn gc_hint(&mut self, _pressure: u8) -> isoplane_engine::DispatchOutcome {
            if let Some(upload) = &self.gc_upload {
                self.pending_upload =
                    Some(upload.begin_read().expect("GC probe starts its EOF read"));
            }
            if self.tombstoned {
                return self.outcome(Completion::Trapped);
            }
            if self.how.tombstones() {
                self.tombstoned = true;
            }
            self.outcome(self.how)
        }
        fn reset(&mut self) -> anyhow::Result<()> {
            anyhow::ensure!(!self.tombstoned, "tombstoned");
            Ok(())
        }
        fn is_parked(&self) -> bool {
            !self.tombstoned
        }
        fn is_capture_eligible(&self) -> bool {
            !self.tombstoned
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
            anyhow::ensure!(!self.tombstoned, "tombstoned");
            Ok(isoplane_engine::InstanceImage {
                memory_pages: (self.memory.len() / isoplane_abi::WASM_PAGE_SIZE) as u32,
                ..Default::default()
            })
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
                template: Arc::new(vec![0u8; isoplane_abi::WASM_PAGE_SIZE]),
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
            Ok(snapshot_size_bound(SnapshotShape {
                max_memory_bytes: self.template.len() as u64,
                mutable_globals: 0,
            })?)
        }
        fn activate(
            &self,
            _b: Box<dyn isoplane_engine::HostBindings>,
        ) -> anyhow::Result<Box<dyn isoplane_engine::InstanceOps + Send>> {
            Ok(Box::new(HintEndsIn {
                how: self.how,
                tombstoned: false,
                memory: (*self.template).clone(),
                gc_upload: None,
                pending_upload: None,
                storage_failed: false,
            }))
        }
    }

    struct NoWake;
    impl crate::bindings::WakeSink for NoWake {
        fn signal(&self, _owner: crate::InstanceId) {}
    }

    fn local_context(
        id: crate::InstanceId,
        lifecycle: isoplane_abi::config::HandlerLifecycle,
        limits: isoplane_abi::config::Limits,
    ) -> crate::bindings::InstanceCtx {
        static REACTOR: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let reactor = REACTOR.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("local fixture reactor")
        });
        let _entered = reactor.enter();
        let mut context = crate::bindings::InstanceCtx::for_test(id, lifecycle, limits);
        context.rt = Some(Arc::new(crate::bindings::CellRuntime {
            runtime_configuration: Default::default(),
            services: Arc::new(crate::bindings::Services {
                egress: Arc::new(
                    crate::egress::HostEgressPolicy::system(0).expect("fixture resolver"),
                ),
                fetcher: Arc::new(crate::fakes::FakeFetcher::default()),
            }),
            futures: Arc::new(crate::futures::FutureTable::new()),
            metrics: Arc::new(crate::metrics::Metrics::default()),
            wake: Arc::new(NoWake),
            reactor: reactor.handle().clone(),
        }));
        context
    }

    fn parked_slot(how: Completion) -> (Arc<Slot>, Arc<dyn Activator>) {
        use isoplane_abi::config::{HandlerLifecycle, Limits};
        let act: Arc<dyn Activator> = Arc::new(HintActivator::new(how));
        let pool = InstancePool::new(act.clone(), HandlerLifecycle::Reuse, 4);
        let (slot, _) = pool
            .acquire(|id| {
                Arc::new(local_context(
                    id,
                    HandlerLifecycle::Reuse,
                    Limits::default(),
                ))
            })
            .expect("a slot");
        let storage = tempfile::tempdir().expect("owned local fixture storage");
        let storage = isoplane_hibernate::snapshot::LocalSnapshotStorage::new(storage.path())
            .expect("open real reservation storage");
        let bytes = snapshot_size_bound(SnapshotShape {
            max_memory_bytes: isoplane_abi::PAGE_SIZE as u64,
            mutable_globals: 0,
        })
        .expect("bound the fixture's actual sandbox");
        *slot.ctx.snapshot.lock() = Some(
            isoplane_hibernate::snapshot::SnapshotStorage::reserve(
                &storage,
                isoplane_hibernate::snapshot::SnapshotBytes(bytes),
            )
            .expect("reserve fixture capacity"),
        );
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
        let futures = slot
            .ctx
            .rt
            .as_ref()
            .expect("the local fixture owns a runtime")
            .futures
            .clone();
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
            "local capture keeps pending I/O pending"
        );
        futures.complete(id, ResultRecord::Ok(b"completed while asleep".to_vec()));
        assert!(
            matches!(futures.take_ready(slot.id), Some((ready, ResultRecord::Ok(bytes))) if ready == id && bytes == b"completed while asleep"),
            "the original operation must deliver its result after local capture"
        );
    }

    #[test]
    fn in_process_capture_retains_ready_results() {
        use isoplane_abi::result::ResultRecord;
        let (slot, act) = parked_slot(Completion::Done);
        let futures = slot
            .ctx
            .rt
            .as_ref()
            .expect("the local fixture owns a runtime")
            .futures
            .clone();
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
            matches!(futures.take_ready(slot.id), Some((ready, ResultRecord::Ok(bytes))) if ready == id && bytes == vec![42; 32]),
            "local capture preserves the exact ready result"
        );
    }

    /// The engine's tombstone and the slot's must agree, or the idle sweep
    /// re-selects a Parked slot every tick and re-enters a killed instance.
    #[test]
    fn a_guest_that_traps_in_its_gc_hint_is_tombstoned_on_the_slot_too() {
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
                slot.is_tombstoned(),
                "{how:?}: the engine tombstoned itself and the slot did not"
            );
            assert!(
                matches!(slot.enter(), Err(crate::slot::EnterError::Tombstoned)),
                "{how:?}: the front door must refuse"
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
                tombstoned: false,
                memory: vec![0; isoplane_abi::WASM_PAGE_SIZE],
                gc_upload: None,
                pending_upload: None,
                storage_failed: true,
            }),
        ));
        slot.park(0);
        assert_eq!(
            try_capture(&slot, &act).err(),
            Some(CaptureRefused::StorageFailed),
            "a delta computed from failed storage must never be kept"
        );
        assert!(
            slot.is_tombstoned(),
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
        assert!(!slot.is_tombstoned(), "parking in the hint is not a fault");
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
        *slot.ctx.request_body.lock() = Some(source.clone());
        let futures = slot
            .ctx
            .rt
            .as_ref()
            .expect("the local fixture owns a runtime")
            .futures
            .clone();
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
            matches!(futures.take_ready(slot.id), Some((ready_id, ResultRecord::Ok(bytes))) if ready_id == id && bytes.is_empty()),
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
        *initial.ctx.request_body.lock() = Some(source.clone());
        let slot = Arc::new(Slot::new(
            initial.id,
            initial.ctx.clone(),
            Box::new(HintEndsIn {
                how: Completion::Done,
                tombstoned: false,
                memory: vec![0; isoplane_abi::WASM_PAGE_SIZE],
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

/// A checked snapshot for the original host context.
pub(crate) use local_snapshot::Snapshot as LocalSnapshot;
pub use local_snapshot::{
    size_bound as snapshot_size_bound, Error as LocalSnapshotError, SnapshotShape,
};

/// Reads and checks only the snapshot owned by this original host context.
pub(crate) fn read_in_context(
    slot: &Arc<Slot>,
    activator: &Arc<dyn Activator>,
) -> anyhow::Result<LocalSnapshot> {
    let mut reservation = slot.ctx.snapshot.lock();
    let reservation = reservation
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("instance has no snapshot reservation"))?;
    let storage_bytes = reservation.reserved_bytes().0;
    let bytes = reservation.load()?;
    Ok(local_snapshot::Snapshot::open(
        bytes,
        local_snapshot::Identity {
            module: activator.module_hash(),
            abi_rev: activator.abi_rev(),
            instance: slot.id,
        },
        local_snapshot::Bounds {
            memory_bytes: slot.ctx.limits.instance_memory_max,
            storage_bytes,
        },
    )?)
}

mod local_snapshot {
    use isoplane_engine::{InstanceImage, MemoryImage, ModuleHash};
    use serde::{Deserialize, Serialize};

    use crate::InstanceId;

    const LENGTH_BYTES: usize = std::mem::size_of::<u32>();
    const CHECKSUM_BYTES: usize = 32;
    const PAGE: usize = isoplane_abi::PAGE_SIZE;

    /// Maximum sandbox memory and mutable globals admitted for one instance.
    #[derive(Clone, Copy)]
    pub struct SnapshotShape {
        /// Maximum reachable linear-memory bytes, including growth.
        pub max_memory_bytes: u64,
        /// Mutable numeric globals exported by the compiled module.
        pub mutable_globals: usize,
    }

    /// Bounds raw changed pages and the widest encoded header without allocating them.
    pub fn size_bound(shape: SnapshotShape) -> Result<u64, Error> {
        use isoplane_engine::snapshot::GlobalValue;
        use postcard::experimental::serialized_size;

        let pages = shape.max_memory_bytes.div_ceil(PAGE as u64);
        let header = Header {
            module: [u8::MAX; 32],
            abi_rev: u32::MAX,
            instance: u64::MAX,
            image: InstanceImage {
                memory_pages: u32::MAX,
                ..InstanceImage::default()
            },
            runs: Vec::new(),
        };
        let run = Run {
            first_page: u32::MAX,
            pages: u32::MAX,
            zero: true,
        };
        let mut bytes = (LENGTH_BYTES + CHECKSUM_BYTES + serialized_size(&header)?) as u64;
        for (count, element_bytes) in [
            (pages, serialized_size(&run)?),
            (
                shape.mutable_globals as u64,
                serialized_size(&GlobalValue::I64(i64::MIN))?,
            ),
        ] {
            let prefix_growth = serialized_size(&count)? - serialized_size(&0_usize)?;
            bytes = bytes
                .checked_add(prefix_growth as u64)
                .and_then(|bytes| bytes.checked_add(count.checked_mul(element_bytes as u64)?))
                .ok_or(Error::Overflow)?;
        }
        pages
            .checked_mul(PAGE as u64)
            .and_then(|payload| bytes.checked_add(payload))
            .ok_or(Error::Overflow)
    }

    /// Identity of one instance's snapshot in its original host context.
    #[derive(Clone, Copy)]
    pub(super) struct Identity {
        pub module: ModuleHash,
        pub abi_rev: u32,
        pub instance: InstanceId,
    }

    /// Bounds supplied by the instance and its storage owner.
    #[derive(Clone, Copy)]
    pub(super) struct Bounds {
        pub memory_bytes: u64,
        pub storage_bytes: u64,
    }

    /// Local snapshot encoding, corruption, identity or capacity failure.
    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        /// The stored bytes, page layout or memory size are invalid.
        #[error("local snapshot is corrupt")]
        Corrupt,
        /// The snapshot's compiled module does not match its owner.
        #[error("local snapshot belongs to another module")]
        ModuleMismatch,
        /// The snapshot's host ABI revision does not match its owner.
        #[error("local snapshot belongs to another ABI revision")]
        AbiMismatch,
        /// The snapshot was captured for a different instance.
        #[error("local snapshot belongs to another instance")]
        InstanceMismatch,
        /// The snapshot exceeds an existing memory or storage bound.
        #[error("local snapshot exceeds its {limit_bytes}-byte {resource} bound")]
        Capacity {
            /// The memory or storage resource that refused the snapshot.
            resource: &'static str,
            /// The admitted byte limit.
            limit_bytes: u64,
        },
        /// A size or page offset cannot be represented.
        #[error("local snapshot size arithmetic overflow")]
        Overflow,
        /// Encoding or decoding the bounded header failed.
        #[error("local snapshot codec: {0}")]
        Codec(#[from] postcard::Error),
    }

    /// Encoded state and the measured number of pages written.
    pub(super) struct Encoded {
        pub bytes: Vec<u8>,
        pub changed_pages: u32,
    }

    #[derive(Serialize, Deserialize)]
    struct Header {
        module: [u8; 32],
        abi_rev: u32,
        instance: u64,
        image: InstanceImage,
        runs: Vec<Run>,
    }

    #[derive(Serialize, Deserialize)]
    struct Run {
        first_page: u32,
        pages: u32,
        zero: bool,
    }

    impl Run {
        fn bytes(&self) -> Result<std::ops::Range<usize>, Error> {
            let start = (self.first_page as usize)
                .checked_mul(PAGE)
                .ok_or(Error::Overflow)?;
            let length = (self.pages as usize)
                .checked_mul(PAGE)
                .ok_or(Error::Overflow)?;
            Ok(start..start.checked_add(length).ok_or(Error::Overflow)?)
        }
    }

    /// Encodes sandbox state; live host resources stay with the instance owner.
    pub(super) fn encode(
        identity: Identity,
        bounds: Bounds,
        image: InstanceImage,
        memory: &[u8],
        template: &MemoryImage,
    ) -> Result<Encoded, Error> {
        if memory.len() as u64 > bounds.memory_bytes {
            return Err(Error::Capacity {
                resource: "memory",
                limit_bytes: bounds.memory_bytes,
            });
        }
        if !memory.len().is_multiple_of(PAGE) {
            return Err(Error::Corrupt);
        }
        if u64::from(image.memory_pages).checked_mul(isoplane_abi::WASM_PAGE_SIZE as u64)
            != Some(memory.len() as u64)
        {
            return Err(Error::Corrupt);
        }
        let mut runs: Vec<Run> = Vec::new();
        let mut changed_pages = 0_u32;
        for (page, bytes) in memory.as_chunks::<PAGE>().0.iter().enumerate() {
            if template.page_eq(page, bytes) {
                continue;
            }
            let page = u32::try_from(page).map_err(|_| Error::Overflow)?;
            changed_pages = changed_pages.checked_add(1).ok_or(Error::Overflow)?;
            let zero = bytes.iter().all(|byte| *byte == 0);
            match runs.last_mut() {
                Some(run)
                    if run.zero == zero && run.first_page.checked_add(run.pages) == Some(page) =>
                {
                    run.pages = run.pages.checked_add(1).ok_or(Error::Overflow)?;
                }
                _ => runs.push(Run {
                    first_page: page,
                    pages: 1,
                    zero,
                }),
            }
        }
        let header = Header {
            module: identity.module.0,
            abi_rev: identity.abi_rev,
            instance: identity.instance.0,
            image,
            runs,
        };
        let encoded_header = postcard::to_allocvec(&header)?;
        let header_bytes = u32::try_from(encoded_header.len()).map_err(|_| Error::Overflow)?;
        let mut total = LENGTH_BYTES
            .checked_add(encoded_header.len())
            .and_then(|bytes| bytes.checked_add(CHECKSUM_BYTES))
            .ok_or(Error::Overflow)?;
        for run in &header.runs {
            if !run.zero {
                total = total
                    .checked_add(run.bytes()?.len())
                    .ok_or(Error::Overflow)?;
            }
        }
        if total as u64 > bounds.storage_bytes {
            return Err(Error::Capacity {
                resource: "storage",
                limit_bytes: bounds.storage_bytes,
            });
        }
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(&header_bytes.to_le_bytes());
        bytes.extend_from_slice(&encoded_header);
        for run in &header.runs {
            if !run.zero {
                bytes.extend_from_slice(memory.get(run.bytes()?).ok_or(Error::Corrupt)?);
            }
        }
        let checksum = blake3::hash(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        Ok(Encoded {
            bytes,
            changed_pages,
        })
    }

    /// A checked snapshot, usable only by the matching original instance owner.
    pub(crate) struct Snapshot {
        header: Header,
        bytes: Vec<u8>,
        payload_start: usize,
    }

    impl Snapshot {
        /// Checks the owner, compatibility, complete checksum and page bounds.
        pub(super) fn open(
            bytes: Vec<u8>,
            identity: Identity,
            bounds: Bounds,
        ) -> Result<Self, Error> {
            if bytes.len() as u64 > bounds.storage_bytes {
                return Err(Error::Capacity {
                    resource: "storage",
                    limit_bytes: bounds.storage_bytes,
                });
            }
            let checksum_at = bytes
                .len()
                .checked_sub(CHECKSUM_BYTES)
                .filter(|offset| *offset >= LENGTH_BYTES)
                .ok_or(Error::Corrupt)?;
            let content = bytes.get(..checksum_at).ok_or(Error::Corrupt)?;
            if blake3::hash(content).as_bytes().as_slice()
                != bytes.get(checksum_at..).ok_or(Error::Corrupt)?
            {
                return Err(Error::Corrupt);
            }
            let prefix: [u8; LENGTH_BYTES] = content
                .get(..LENGTH_BYTES)
                .ok_or(Error::Corrupt)?
                .try_into()
                .map_err(|_| Error::Corrupt)?;
            let payload_start = LENGTH_BYTES
                .checked_add(u32::from_le_bytes(prefix) as usize)
                .filter(|offset| *offset <= checksum_at)
                .ok_or(Error::Corrupt)?;
            let header: Header = postcard::from_bytes(
                content
                    .get(LENGTH_BYTES..payload_start)
                    .ok_or(Error::Corrupt)?,
            )?;
            if header.module != identity.module.0 {
                return Err(Error::ModuleMismatch);
            }
            if header.abi_rev != identity.abi_rev {
                return Err(Error::AbiMismatch);
            }
            if header.instance != identity.instance.0 {
                return Err(Error::InstanceMismatch);
            }
            let memory_bytes = u64::from(header.image.memory_pages)
                .checked_mul(isoplane_abi::WASM_PAGE_SIZE as u64)
                .ok_or(Error::Overflow)?;
            if memory_bytes > bounds.memory_bytes {
                return Err(Error::Capacity {
                    resource: "memory",
                    limit_bytes: bounds.memory_bytes,
                });
            }
            let mut previous_end = 0;
            let mut payload_bytes = 0_usize;
            for run in &header.runs {
                let range = run.bytes()?;
                if run.pages == 0 || range.start < previous_end || range.end as u64 > memory_bytes {
                    return Err(Error::Corrupt);
                }
                previous_end = range.end;
                if !run.zero {
                    payload_bytes = payload_bytes
                        .checked_add(range.len())
                        .ok_or(Error::Overflow)?;
                }
            }
            if payload_start.checked_add(payload_bytes) != Some(checksum_at) {
                return Err(Error::Corrupt);
            }
            Ok(Self {
                header,
                bytes,
                payload_start,
            })
        }

        /// Sandbox globals, tables and memory size captured at the return boundary.
        pub(crate) fn image(&self) -> &InstanceImage {
            &self.header.image
        }

        /// Applies checked pages to an activation restored from the same template.
        pub(crate) fn apply(&self, memory: &mut [u8]) -> Result<(), Error> {
            if u64::from(self.header.image.memory_pages)
                .checked_mul(isoplane_abi::WASM_PAGE_SIZE as u64)
                != Some(memory.len() as u64)
            {
                return Err(Error::Corrupt);
            }
            let mut offset = self.payload_start;
            for run in &self.header.runs {
                let target = memory.get_mut(run.bytes()?).ok_or(Error::Corrupt)?;
                if run.zero {
                    target.fill(0);
                } else {
                    let end = offset.checked_add(target.len()).ok_or(Error::Overflow)?;
                    let source = self.bytes.get(offset..end).ok_or(Error::Corrupt)?;
                    target.copy_from_slice(source);
                    offset = end;
                }
            }
            Ok(())
        }
    }
}
