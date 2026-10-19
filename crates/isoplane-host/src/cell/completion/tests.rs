//! Canonical dispatch reporting through the cell's real execution boundaries.

use super::test_events::Events;
use super::*;
use crate::cell::pool::cell_tests::{app, cell, http, manifest, wait_for};
use crate::fakes::*;

/// Stands in for a host assertion firing during a slice.
struct PanicsOnDispatch {
    first: Option<Box<dyn isoplane_engine::InstanceOps + Send>>,
    end_clock_unavailable: bool,
    start_clock_unavailable: bool,
}

#[derive(Clone, Copy)]
enum FaultPoint {
    Hint,
    EndClock,
    StartClock,
    AcquireIo,
    Dispatch,
    Wake,
    AcquireError,
    AcquirePanic,
}

impl isoplane_engine::InstanceOps for PanicsOnDispatch {
    fn dispatch(
        &mut self,
        _rec: &DispatchRecord,
        _b: OccupancyBudget,
    ) -> isoplane_engine::DispatchOutcome {
        if let Some(first) = &mut self.first {
            let mut outcome = first.dispatch(_rec, _b);
            if self.end_clock_unavailable {
                outcome.occupancy_ns = 0;
                outcome.occupancy_measurement = isoplane_engine::OccupancyMeasurement::Incomplete;
            }
            if self.start_clock_unavailable {
                outcome.occupancy_ns = 0;
                outcome.wall_ns = 0;
                outcome.occupancy_measurement = isoplane_engine::OccupancyMeasurement::NotExecuted;
                outcome.completion = Completion::Trapped;
                outcome.status = isoplane_abi::status::PollStatus::Trapped {
                    code: TrapCode::Unknown,
                };
                outcome.trap = Some(isoplane_engine::TrapInfo {
                    kind: isoplane_engine::TrapKind::OccupancyClockUnavailable,
                    message: "controlled clock refusal".into(),
                });
            }
            return outcome;
        }
        panic!("a host assertion fired underneath a dispatch");
    }
    fn wake(
        &mut self,
        _d: u64,
        _f: u64,
        _r: &isoplane_abi::result::ResultRecord,
        _b: OccupancyBudget,
    ) -> isoplane_engine::DispatchOutcome {
        panic!("a host assertion fired underneath a wake");
    }
    fn gc_hint(&mut self, _p: u8) -> isoplane_engine::DispatchOutcome {
        unreachable!("this fixture never reaches the hibernation path")
    }
    fn is_parked(&self) -> bool {
        self.first
            .as_ref()
            .is_some_and(|instance| instance.is_parked())
    }
    fn is_capture_eligible(&self) -> bool {
        self.first
            .as_ref()
            .is_some_and(|instance| instance.is_capture_eligible())
    }
    fn memory(&mut self) -> &mut [u8] {
        &mut []
    }
    fn memory_len(&self) -> usize {
        0
    }
    fn storage_failed(&self) -> bool {
        false
    }
    fn capture_mutable_state(&mut self) -> anyhow::Result<isoplane_engine::InstanceImage> {
        Ok(Default::default())
    }
    fn restore_mutable_state(&mut self, _i: &isoplane_engine::InstanceImage) -> anyhow::Result<()> {
        Ok(())
    }
}

struct PanicActivator {
    module: ModuleId,
    template: Arc<Vec<u8>>,
    fault: FaultPoint,
}

impl Activator for PanicActivator {
    fn module(&self) -> &ModuleId {
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
        let first = match self.fault {
            FaultPoint::AcquireIo => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "private activation detail",
                )
                .into())
            }
            FaultPoint::EndClock | FaultPoint::StartClock => {
                Some(FakeActivator::new(FakeBehaviour::done()).activate(_b)?)
            }
            FaultPoint::AcquireError => anyhow::bail!("controlled activation refusal"),
            FaultPoint::AcquirePanic => panic!("controlled activation panic"),
            FaultPoint::Dispatch => None,
            FaultPoint::Wake | FaultPoint::Hint => {
                let mut behaviour = FakeBehaviour::program([Step::Park {
                    imports: vec![(
                        "clock_deadline_in",
                        if matches!(self.fault, FaultPoint::Hint) {
                            600_000
                        } else {
                            0
                        },
                    )],
                }]);
                behaviour.slice_occupancy = Some(Duration::from_nanos(11));
                Some(FakeActivator::new(behaviour).activate(_b)?)
            }
        };
        Ok(Box::new(PanicsOnDispatch {
            first,
            end_clock_unavailable: matches!(self.fault, FaultPoint::EndClock),
            start_clock_unavailable: matches!(self.fault, FaultPoint::StartClock),
        }))
    }
}

pub(in crate::cell) fn panic_activator(module: ModuleId) -> Arc<dyn Activator> {
    Arc::new(PanicActivator {
        module,
        template: Arc::new(vec![0; isoplane_abi::PAGE_SIZE]),
        fault: FaultPoint::Dispatch,
    })
}

#[test]
fn completion_events_keep_queue_and_park_wall_separate_from_hot_occupancy() {
    let cell = cell(CellConfig {
        exec_threads: 0,
        ..CellConfig::for_test("completion-clock")
    });
    let now = Arc::new(AtomicU64::new(100));
    let clock = now.clone();
    *cell.completion_clock.write() = Arc::new(move || Some(clock.load(Ordering::SeqCst)));
    let mut behaviour = FakeBehaviour::program([
        Step::Park {
            imports: vec![("clock_deadline_in", 600_000)],
        },
        Step::Park {
            imports: vec![("clock_deadline_in", 600_000)],
        },
        Step::Done {
            status: 200,
            body: vec![],
        },
    ]);
    behaviour.slice_occupancy = Some(Duration::from_nanos(11));
    let module = app(&cell, "completion-clock", behaviour);
    let events = Events::default();
    let rx = events
        .scope(|| cell.submit(&module, Target::Handler { route: "/".into() }, http(42)))
        .expect("admitted");
    now.store(200, Ordering::SeqCst);
    let Job::Dispatch(job) = cell.queue.pop(0).expect("queued dispatch") else {
        panic!("expected dispatch");
    };
    cell.run_dispatch(*job);
    assert!(
        events.records().is_empty(),
        "a parked slice is not dispatch completion"
    );
    let slot = cell.slot(cell.resident_ids()[0]).expect("parked owner");
    for end in [400, 900] {
        now.store(end, Ordering::SeqCst);
        let future = cell
            .futures()
            .live_guest_future_ids()
            .into_iter()
            .next()
            .expect("owned timer");
        cell.complete_future(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
        cell.run_wake(slot.clone());
        if end == 400 {
            assert!(
                events.records().is_empty(),
                "a second park must retain the original event owner"
            );
        }
    }
    let response = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("completed response");
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "all hot slices must produce one terminal event"
    );
    assert_eq!(
        records[0]["slice_occupancy_ns"], 33,
        "each prescribed hot slice must contribute occupancy exactly once"
    );
    assert_eq!(
        records[0]["slices"], 3,
        "the completion must span both parks"
    );
    assert_eq!(
        records[0]["total_wall_ns"], 800,
        "total wall must include queueing and both parked intervals from original admission"
    );
    assert!(
        response.occupancy_ns >= 33,
        "the response retains all slice usage"
    );
    assert_eq!(
        cell.meters(&module).expect("meters").occupancy_ns(),
        records[0]["occupancy_ns"]
            .as_u64()
            .expect("total occupancy"),
        "canonical reporting must not double-count slices"
    );
    cell.shutdown();
}

#[test]
fn unavailable_admission_clock_is_reported_as_missing_total_wall() {
    let cell = cell(CellConfig {
        exec_threads: 1,
        ..CellConfig::for_test("completion-no-clock")
    });
    *cell.completion_clock.write() = Arc::new(|| None);
    let module = app(&cell, "completion-no-clock", FakeBehaviour::done());
    let events = Events::default();
    events
        .scope(|| {
            cell.dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                http(1),
                Duration::from_secs(10),
            )
        })
        .expect("completed");
    cell.shutdown();
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "missing time must not erase the dispatch outcome"
    );
    assert_eq!(
        records[0]["total_wall_complete"], false,
        "an unreadable clock is not a measured zero"
    );
    assert!(
        !records[0].contains_key("total_wall_ns"),
        "unavailable total wall must remain absent"
    );
}

#[test]
fn registration_refuses_missing_tenant_identity_before_guest_execution() {
    let cell = cell(CellConfig::for_test("completion-no-identity"));
    let result = cell.register_module(
        " ",
        Arc::new(FakeActivator::new(FakeBehaviour::done())),
        manifest("missing-owner"),
    );
    assert!(
        result.is_err(),
        "a dispatch must never inherit default or blank tenant identity"
    );
    assert!(
        cell.resident_ids().is_empty(),
        "missing identity must fail before activation"
    );
    cell.shutdown();
}

fn recorded_fault(
    fault: FaultPoint,
) -> (
    DispatchResult,
    Vec<serde_json::Map<String, serde_json::Value>>,
) {
    let cell = cell(CellConfig {
        exec_threads: 1,
        ..CellConfig::for_test("recorded-fault")
    });
    let module = ModuleId::new("recorded-fault", "v1");
    cell.register_module(
        "acme",
        Arc::new(PanicActivator {
            module: module.clone(),
            template: Arc::new(vec![0; isoplane_abi::PAGE_SIZE]),
            fault,
        }),
        manifest("recorded-fault"),
    )
    .expect("register fault fixture");
    let events = Events::default();
    let result = events
        .scope(|| {
            cell.dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                http(7),
                Duration::from_secs(10),
            )
        })
        .expect("every handled fault must answer its caller");
    assert!(
        wait_for(
            || !cell.active.contains_key(&result.instance),
            Duration::from_secs(10)
        ),
        "fault finalization must release the active owner before shutdown"
    );
    assert!(
        cell.futures().live_guest_future_ids().is_empty(),
        "terminal fault cleanup must release owned futures before shutdown"
    );
    if matches!(fault, FaultPoint::EndClock) {
        assert_eq!(
            cell.meters(&module)
                .expect("registered module meters")
                .incomplete_intervals(),
            1,
            "an incomplete engine interval must be counted rather than recorded as a complete zero"
        );
    }
    cell.shutdown();
    (result, events.completed(1))
}

#[test]
fn an_acquisition_error_emits_one_attributed_completion_without_an_instance() {
    let (result, records) = recorded_fault(FaultPoint::AcquireError);
    assert_eq!(
        result.status, 500,
        "activation refusal must fail the dispatch"
    );
    assert_eq!(
        records.len(),
        1,
        "activation refusal must have one completion owner"
    );
    assert_eq!(
        records[0]["outcome"], "acquisition_failed",
        "activation failure must remain distinct from a guest trap"
    );
    assert_eq!(
        records[0]["tenant"], "acme",
        "identity must survive failure before slot acquisition"
    );
    assert!(
        !records[0].contains_key("instance"),
        "no acquired instance means absent identity, never instance zero"
    );
}

#[test]
fn an_acquisition_panic_is_contained_and_completes_before_a_slot_exists() {
    let (result, records) = recorded_fault(FaultPoint::AcquirePanic);
    assert_eq!(
        result.status, 500,
        "a caught activation panic must answer the caller"
    );
    assert_eq!(
        records.len(),
        1,
        "activation panic must emit exactly one completion"
    );
    assert_eq!(
        records[0]["outcome"], "host_panic",
        "the owning boundary must report caught unwinding"
    );
    assert_eq!(
        records[0]["occupancy_complete"], true,
        "no slice ran before activation failed"
    );
    assert!(
        !records[0].contains_key("instance"),
        "an activation panic cannot invent an instance identity"
    );
}

#[test]
fn a_wake_panic_preserves_prior_slice_occupancy_and_marks_the_interrupted_slice_incomplete() {
    let (result, records) = recorded_fault(FaultPoint::Wake);
    assert!(
        result.occupancy_ns >= 11,
        "panic response preserves prior slice usage"
    );
    assert_eq!(
        records.len(),
        1,
        "park and caught wake panic must share a completion owner"
    );
    assert_eq!(
        records[0]["slice_occupancy_ns"], 11,
        "the canonical occupancy floor must retain the first slice"
    );
    assert_eq!(
        records[0]["occupancy_complete"], false,
        "the interrupted wake cannot be recorded as a complete zero"
    );
    assert_eq!(
        records[0]["slice_wall_complete"], false,
        "the interrupted interval must also mark hot wall incomplete"
    );
    assert_eq!(
        records[0]["outcome"], "host_panic",
        "the terminal reason is the caught wake fault"
    );
}

fn real_completion_cell(wasm: Vec<u8>, slice_wall_ms_cap: u32) -> (Arc<Cell>, ModuleId) {
    let engine = Arc::new(
        isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real completion engine"),
    );
    let module = ModuleId::new("real-completion", "v1");
    let activator = Arc::new(
        crate::EngineActivator::load(engine, module.clone(), &wasm).expect("real completion guest"),
    );
    let cell = cell(CellConfig {
        exec_threads: 1,
        slice_wall_ms_cap,
        ..CellConfig::for_test("real-completion")
    });
    cell.register_module("acme", activator, manifest("real-completion"))
        .expect("register real engine");
    (cell, module)
}

fn real_completion_wasm(imports: &str, data: &str, body: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {imports} (memory (export "memory") 2 2) {data}
            (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
            (func (export "isoplane_handle") (param i32 i32) (result i64) {body})
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) unreachable))"#
    ))
    .expect("real completion WAT")
}

fn real_completion_outcome(
    body: &str,
    occupancy_ms: u32,
    slice_wall_ms_cap: u32,
) -> serde_json::Map<String, serde_json::Value> {
    let (cell, module) =
        real_completion_cell(real_completion_wasm("", "", body), slice_wall_ms_cap);
    let events = Events::default();
    let record = DispatchRecord {
        occupancy_budget_ms: occupancy_ms,
        ..http(123)
    };
    events
        .scope(|| {
            cell.dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                record,
                Duration::from_secs(10),
            )
        })
        .expect("real engine dispatch must end");
    cell.shutdown();
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "one real guest dispatch must emit exactly one canonical completion"
    );
    assert_eq!(
        records[0]["tenant"], "acme",
        "engine outcomes must carry established ownership"
    );
    assert_eq!(
        records[0]["dispatch_id"], 123,
        "the caller's correlation ID must survive execution"
    );
    records.into_iter().next().expect("the asserted completion")
}

#[test]
fn real_guest_success_has_one_canonical_completion() {
    let event =
        real_completion_outcome("i64.const 0", 1000, CellConfig::default().slice_wall_ms_cap);
    assert_eq!(
        event["outcome"], "success",
        "a normal empty-stack return must complete successfully"
    );
    assert_eq!(
        event["occupancy_complete"], true,
        "the real slice must have a complete occupancy reading"
    );
}

#[test]
fn real_guest_trap_has_one_canonical_failure() {
    let event =
        real_completion_outcome("unreachable", 1000, CellConfig::default().slice_wall_ms_cap);
    assert_eq!(
        event["outcome"], "guest_trap",
        "a sandbox trap must be recorded at the terminal boundary"
    );
}

#[test]
fn real_occupancy_hard_kill_is_distinct_from_the_execution_wall_backstop() {
    let event = real_completion_outcome(
        "(loop br 0) unreachable",
        0,
        CellConfig::default().slice_wall_ms_cap,
    );
    assert_eq!(
        event["outcome"], "occupancy_limit",
        "the real occupancy hard guard must preserve its typed reason"
    );
    assert_eq!(
        event["status"], 503,
        "budget exhaustion must fail the dispatch"
    );
}

#[test]
fn a_real_zero_budget_dispatch_has_one_budget_completion() {
    let event = real_completion_outcome("i64.const 0", 0, CellConfig::default().slice_wall_ms_cap);
    assert_eq!(
        event["outcome"], "occupancy_limit",
        "returning at the zero occupancy budget is a clean budget outcome"
    );
    assert_eq!(
        event["status"], 503,
        "clean unwinding must not turn budget exhaustion into success"
    );
}

#[test]
fn real_zero_wall_backstop_reports_execution_wall_limit() {
    let event = real_completion_outcome("(loop br 0) unreachable", 1000, 0);
    assert_eq!(
        event["outcome"], "execution_wall_limit",
        "a wall backstop must never be relabelled occupancy exhaustion"
    );
    assert_eq!(
        event["slice_wall_cap_ns"], 0,
        "the event must carry the configured bound that fired"
    );
}

async fn ended_real_guest_response(transport_failure: bool) {
    let head = isoplane_abi::encode(&isoplane_abi::dispatch::ResponseHead {
        status: 200,
        headers: vec![],
        body: b"open".to_vec(),
        streaming: true,
    })
    .expect("response contract");
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    let wasm = real_completion_wasm(
            r#"(import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
            (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
            &format!(r#"(data (i32.const 1024) "{encoded}")"#),
            &format!("(drop (call $response (i32.const 1024) (i32.const {}))) (drop (call $timer (i64.const 600000))) i64.const 1", head.len()));
    let (cell, module) = real_completion_cell(wasm, CellConfig::default().slice_wall_ms_cap);
    let stream = crate::response_transport::ResponseTransport::new(1);
    let events = Events::default();
    let task = {
        let cell = cell.clone();
        let module = module.clone();
        let stream = stream.clone();
        let events = events.clone();
        tokio::task::spawn_blocking(move || {
            events.scope(|| {
                cell.dispatch_with_stream(
                    &module,
                    Target::Handler { route: "/".into() },
                    http(19),
                    Duration::from_secs(10),
                    Some(stream),
                )
            })
        })
    };
    let exchange = stream.exchange().await.expect("real response headers");
    assert!(
        events.records().is_empty(),
        "response headers must not complete a streaming dispatch"
    );
    let id = cell.resident_ids()[0];
    assert!(
        matches!(
            cell.force_hibernate(id),
            Err(hibernate_policy::CaptureRefused::OpenResponse)
        ),
        "an open requester must exercise ordinary nonterminal capture refusal"
    );
    assert!(
        events.records().is_empty(),
        "ordinary capture refusal must not finalize the dispatch"
    );
    if transport_failure {
        stream.fail("controlled transport failure".into());
    }
    drop(exchange);
    let result = task
        .await
        .expect("dispatch task")
        .expect("request cancelled at host boundary");
    assert_eq!(
        result.status,
        if transport_failure { 502 } else { 499 },
        "the terminal transport producer must determine the response status"
    );
    // The client is answered at once; the dispatch ends when its guest stops.
    let records = events.completed(1);
    cell.shutdown();
    assert_eq!(
        records.len(),
        1,
        "late response writer cleanup must not duplicate cancellation completion"
    );
    assert_eq!(
        records[0]["outcome"],
        if transport_failure {
            "transport_failure"
        } else {
            "request_cancelled"
        },
        "the terminal transport producer must determine the completion outcome"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "request cancellation must remove the unused timer wake"
    );
}

#[test]
fn a_real_trapping_capture_hint_completes_its_active_dispatch() {
    let events = Events::default();
    let _trace = events.install();
    let wasm = real_completion_wasm(
        r#"(import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))"#,
        r#"(func (export "isoplane_gc_hint") (param i32) unreachable)"#,
        "(drop (call $timer (i64.const 600000))) i64.const 1",
    );
    let (cell, module) = real_completion_cell(wasm, CellConfig::default().slice_wall_ms_cap);
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(201))
        .expect("accepted dispatch");
    assert!(
        wait_for(|| !cell.resident_ids().is_empty(), Duration::from_secs(10)),
        "the real guest must acquire a slot"
    );
    let id = cell.resident_ids()[0];
    let result = cell.force_hibernate(id);
    assert!(
        matches!(
            result,
            Err(hibernate_policy::CaptureRefused::GcHintDidNotComplete(
                Completion::Trapped
            ))
        ),
        "the real capture hint must exercise the terminal trap path"
    );
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(10))
            .expect("capture failure must answer")
            .status,
        500,
        "a trapping hint cannot leave the requester parked"
    );
    cell.shutdown();
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "a hint trap must finalize the original owner exactly once"
    );
    let event = &records[0];
    assert_eq!(
        event["outcome"], "capture_failed",
        "capture failure is a terminal dispatch outcome"
    );
    assert_eq!(
        event["error_kind"], "guest_panic",
        "the canonical failure must retain the real engine's typed trap kind"
    );
    assert_eq!(
        event["capture_hints"], 1,
        "the report must include the guest work attempted by capture"
    );
    assert_eq!(
        event["capture_hint_complete"], true,
        "a returned trapping hint still has a complete occupancy measurement"
    );
    assert_eq!(
        event["occupancy_ns"].as_u64(),
        Some(cell.meters(&module).expect("meters").occupancy_ns()),
        "hint diagnostics must not silently alter purchased dispatch occupancy accounting"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "capture failure must release the abandoned timer"
    );
}

#[test]
fn a_panicking_capture_hint_marks_its_usage_incomplete_and_releases_the_dispatch() {
    let events = Events::default();
    let _trace = events.install();
    let cell = cell(CellConfig {
        exec_threads: 1,
        ..CellConfig::for_test("hint-panic")
    });
    let module = ModuleId::new("hint-panic", "v1");
    cell.register_module(
        "acme",
        Arc::new(PanicActivator {
            module: module.clone(),
            template: Arc::new(vec![0; isoplane_abi::PAGE_SIZE]),
            fault: FaultPoint::Hint,
        }),
        manifest("hint-panic"),
    )
    .expect("register hint fault");
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(202))
        .expect("accepted");
    assert!(
        wait_for(|| !cell.resident_ids().is_empty(), Duration::from_secs(10)),
        "the hint owner must become resident"
    );
    assert!(
        cell.force_hibernate(cell.resident_ids()[0]).is_err(),
        "the hint panic must fail capture without escaping containment"
    );
    assert!(
        response
            .recv_timeout(Duration::from_secs(10))
            .expect("panic response")
            .occupancy_ns
            >= 11,
        "a failed hint retains prior slice usage"
    );
    cell.shutdown();
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "the capture boundary must complete the original dispatch once"
    );
    assert_eq!(
        records[0]["outcome"], "host_panic",
        "the canonical reason must distinguish caught host unwinding"
    );
    assert_eq!(
        records[0]["capture_hint_complete"], false,
        "an interrupted hint cannot report a complete zero measurement"
    );
    assert_eq!(
        records[0]["occupancy_complete"], true,
        "the separate dispatch slice was completely measured"
    );
    assert_eq!(
        records[0]["total_guest_occupancy_complete"], false,
        "diagnostic guest occupancy must expose the interrupted hint"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "the failed capture must end all owned timer work"
    );
}

#[test]
fn shutdown_completes_accepted_queued_work_without_inventing_an_instance() {
    let cell = cell(CellConfig {
        exec_threads: 0,
        queue_depth_per_worker: 1,
        ..CellConfig::for_test("queued-stop")
    });
    let module = app(&cell, "queued-stop", FakeBehaviour::done());
    let events = Events::default();
    let _trace = events.install();
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(301))
        .expect("the first dispatch is accepted");
    assert!(
        cell.submit(&module, Target::Handler { route: "/".into() }, http(302))
            .is_none(),
        "the second dispatch must exercise rejected admission"
    );
    assert!(
        events.records().is_empty(),
        "admission refusal must not fabricate a completed dispatch"
    );
    cell.shutdown();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(10))
            .expect("queued cancellation answer")
            .status,
        503,
        "accepted queued work must not be abandoned on stop"
    );
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "only accepted queued work gets a terminal dispatch record"
    );
    assert_eq!(
        records[0]["dispatch_id"], 301,
        "the accepted owner must survive queue shutdown"
    );
    assert_eq!(
        records[0]["outcome"], "cell_stopped",
        "orderly stop must name the cancellation source"
    );
    assert!(
        !records[0].contains_key("instance"),
        "queued cancellation must not invent instance zero"
    );
}

#[test]
fn shutdown_ends_parked_owners_and_late_futures_cannot_complete_them_again() {
    let events = Events::default();
    let _trace = events.install();
    let cell = cell(CellConfig {
        exec_threads: 0,
        ..CellConfig::for_test("parked-stop")
    });
    let module = app(
        &cell,
        "parked-stop",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", 600_000)],
        }]),
    );
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(303))
        .expect("accepted");
    let Job::Dispatch(job) = cell.queue.pop(0).expect("queued owner") else {
        panic!("expected dispatch");
    };
    cell.run_dispatch(*job);
    let future = cell.futures().live_guest_future_ids()[0];
    assert!(
        events.records().is_empty(),
        "parking retains the completion owner"
    );
    cell.shutdown();
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(10))
            .expect("parked cancellation response")
            .status,
        503,
        "shutdown must answer its parked requester"
    );
    cell.complete_future(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
    assert_eq!(
        events.completed(1).len(),
        1,
        "a late timer completion cannot duplicate orderly cancellation"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "shutdown cannot leave owned reactor work running"
    );
}

#[test]
fn a_worker_that_loses_its_cell_still_finalizes_accepted_queued_owners() {
    let events = Events::default();
    let _trace = events.install();
    let cell = cell(CellConfig {
        exec_threads: 0,
        ..CellConfig::for_test("lost-cell")
    });
    let module = app(&cell, "lost-cell", FakeBehaviour::done());
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(304))
        .expect("accepted");
    cell.queue.close();
    Cell::exec_loop(Weak::new(), cell.queue.clone(), 0);
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(10))
            .expect("weak owner cancellation response")
            .status,
        503,
        "weak Cell loss must not abandon an accepted caller"
    );
    cell.shutdown();
    assert_eq!(
        events.completed(1).len(),
        1,
        "the weak-cell branch must emit one terminal record"
    );
}

#[test]
fn an_unavailable_engine_endpoint_cannot_report_complete_zero_dispatch_occupancy() {
    let (result, records) = recorded_fault(FaultPoint::EndClock);
    assert_eq!(
        result.occupancy_ns, 0,
        "a failed endpoint can return only the known occupancy floor"
    );
    assert_eq!(
        records.len(),
        1,
        "measurement failure cannot erase terminal completion"
    );
    assert_eq!(
        records[0]["occupancy_complete"], false,
        "the host must consume engine measurement validity rather than certify a complete zero"
    );
    assert_eq!(
        records[0]["occupancy_scope"], "dispatch_slices_and_reactor_polls",
        "occupancy completeness must name its measured scope"
    );
    assert_eq!(
        records[0]["activation_occupancy_included"], false,
        "activation usage remains outside the slice meter"
    );
}

#[test]
fn activation_io_errors_keep_safe_typed_context_without_raw_error_text() {
    let (_, records) = recorded_fault(FaultPoint::AcquireIo);
    assert_eq!(
        records.len(),
        1,
        "the acquisition error must have one canonical record"
    );
    assert_eq!(
        records[0]["error_kind"], "host_io",
        "typed I/O errors must retain their category"
    );
    assert_eq!(
        records[0]["io_kind"], "NotFound",
        "the canonical record must explain the typed acquisition refusal"
    );
    assert_eq!(
        records[0]["error_classification_complete"], true,
        "the I/O kind was established without parsing message text"
    );
    assert!(
        !serde_json::to_string(&records)
            .expect("event JSON")
            .contains("private activation detail"),
        "completion events must not copy arbitrary activation error text"
    );
}

#[test]
fn a_discarded_wake_ends_its_owned_dispatch() {
    let events = Events::default();
    let _trace = events.install();
    let cell = cell(CellConfig {
        exec_threads: 0,
        ..CellConfig::for_test("refused-wake")
    });
    let module = app(
        &cell,
        "refused-wake",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", 600_000)],
        }]),
    );
    let response = cell
        .submit(&module, Target::Handler { route: "/".into() }, http(401))
        .expect("accepted dispatch");
    let Job::Dispatch(job) = cell.queue.pop(0).expect("queued dispatch") else {
        panic!("expected dispatch");
    };
    cell.run_dispatch(*job);
    let slot = cell.slot(cell.resident_ids()[0]).expect("parked slot");
    let measured_occupancy_ns = cell
        .first_active(slot.id, |dispatch| dispatch.completion.usage().occupancy_ns)
        .expect("active owner usage");
    slot.discard();
    cell.run_wake(slot.clone());
    let result = response
        .recv_timeout(Duration::from_secs(10))
        .expect("refused wake answer");
    assert_eq!(
        result.status, 409,
        "terminal entry refusal must answer the parked requester"
    );
    assert!(
        result.occupancy_ns >= measured_occupancy_ns,
        "entry refusal preserves usage, including any reactor work completed since parking"
    );
    assert!(
        !cell.active.contains_key(&slot.id),
        "terminal refusal must remove its active owner"
    );
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "terminal refusal must release owned timers"
    );
    cell.run_wake(slot);
    let records = events.completed(1);
    assert_eq!(
        records.len(),
        1,
        "late wakes must not duplicate entry refusal"
    );
    assert_eq!(
        records[0]["outcome"], "entry_refused",
        "entry refusal must name the terminal boundary"
    );
    cell.shutdown();
}

#[tokio::test]
async fn dropping_a_real_guest_response_emits_one_cancelled_completion() {
    ended_real_guest_response(false).await;
}

#[tokio::test]
async fn a_real_guest_response_transport_failure_emits_one_failed_completion() {
    ended_real_guest_response(true).await;
}

#[test]
fn a_clock_entry_refusal_reports_no_guest_execution() {
    let (result, records) = recorded_fault(FaultPoint::StartClock);
    assert_eq!(
        records.len(),
        1,
        "clock refusal must finalize its accepted owner once"
    );
    assert_eq!(
        records[0]["outcome"], "occupancy_clock_unavailable",
        "the typed clock refusal must survive host classification"
    );
    assert_eq!(
        records[0]["occupancy_ns"], 0,
        "refusal before guest entry has no slice occupancy"
    );
    assert_eq!(
        records[0]["occupancy_complete"], true,
        "known absence of a slice is distinct from missing measurement"
    );
    assert_eq!(
        result.occupancy_ns, 0,
        "the caller and event must share known slice usage"
    );
}

#[test]
fn completion_includes_the_final_reactor_poll_even_when_that_poll_finishes_the_dispatch() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let cell = cell(CellConfig {
        exec_threads: 0,
        ..CellConfig::for_test("final-reactor-poll")
    });
    let module = app(&cell, "final-reactor-poll", FakeBehaviour::done());
    let events = Events::default();
    let owner = events
        .scope(|| cell.completion_owner(&module, &http(1)))
        .expect("completion owner");
    let meters = cell.meters(&module).expect("tenant meters");
    let task = crate::reactor_accounting::TaskUsage::new(meters.clone(), Some(owner.clone()));
    let mut future = Box::pin(task.measure(async {
        owner.select(TerminalReason::Success, 200);
        owner.finish();
        assert!(
            events.records().is_empty(),
            "the current reactor poll is not accounted yet"
        );
    }));
    crate::occupancy_clock::with_readings([100, 150], || {
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(())
        );
    });
    drop(future);
    let records = events.completed(1);
    assert_eq!(records[0]["occupancy_ns"], 50);
    assert_eq!(records[0]["reactor_occupancy_ns"], 50);
    assert_eq!(records[0]["slice_occupancy_ns"], 0);
    assert_eq!(meters.occupancy_ns(), 50);
    owner.finish();
    assert_eq!(events.records().len(), 1);
    cell.shutdown();
}
