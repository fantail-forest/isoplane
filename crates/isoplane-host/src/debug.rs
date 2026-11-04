//! Local debugger activation using the cell's ordinary dispatch and cleanup.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::dispatch::DispatchRecord;
use isoplane_abi::native_io::{Error, Request, Response};
use isoplane_abi::result::ResultRecord;
use isoplane_engine::{
    Completion, DispatchOutcome, HostBindings, ImportCall, Instance, InstanceImage, InstanceOps,
    IsoplaneEngine, MemoryImage, ModuleHash, OccupancyBudget, OccupancyMeasurement,
};

use crate::{Activator, EngineActivator, ModuleId};

/// Guest execution backstop for local debugging.
pub const DEBUG_BUDGET_MS: u32 = u32::MAX;
const READINESS_INTERVAL: Duration = Duration::from_millis(1);

/// Failure to construct a raw local debugger guest.
#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    /// Debugging requires the raw runtime initialization export.
    #[error("local debugger module has no initialization export")]
    MissingInitialize,
}

/// Loads fresh raw instances; initialization executes inside the first accounted dispatch.
/// Instances refuse capture, including capture after an ordinary await.
pub struct DebugActivator {
    engine: EngineActivator,
    stopped: Arc<AtomicBool>,
}

impl DebugActivator {
    /// Activates `template`, the compiled raw module, without pre-initializing
    /// or cloning debugger session state. Fails unless the module still has
    /// its initializer to run.
    pub fn new(
        engine: Arc<IsoplaneEngine>,
        module: ModuleId,
        template: isoplane_engine::Template,
    ) -> Result<Self, DebugError> {
        let inner = EngineActivator::new(engine, template, module);
        if !inner.template().has_initialize() {
            return Err(DebugError::MissingInitialize);
        }
        Ok(Self {
            engine: inner,
            stopped: Arc::new(AtomicBool::new(false)),
        })
    }
    /// Ends debugger readiness waits so the local host can join its workers.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
}

impl Activator for DebugActivator {
    fn module(&self) -> &ModuleId {
        self.engine.module()
    }
    fn module_hash(&self) -> ModuleHash {
        self.engine.module_hash()
    }
    fn abi_rev(&self) -> u32 {
        self.engine.abi_rev()
    }
    fn imports(&self, module: &str, name: &str) -> bool {
        self.engine.imports(module, name)
    }
    fn template_image(&self) -> Arc<MemoryImage> {
        self.engine.template_image()
    }
    fn warm_page_cache(&self) -> anyhow::Result<()> {
        self.engine.warm_page_cache()
    }
    fn snapshot_size_bound(&self, max_memory_bytes: u64) -> anyhow::Result<u64> {
        self.engine.snapshot_size_bound(max_memory_bytes)
    }
    fn activate(
        &self,
        bindings: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let instance = self
            .engine
            .activate_raw(Box::new(DebugBindings(bindings, self.stopped.clone())))?;
        Ok(Box::new(DebugInstance {
            instance,
            initialized: false,
            pending_dispatch: None,
        }))
    }
}

struct DebugBindings(Box<dyn HostBindings>, Arc<AtomicBool>);
impl HostBindings for DebugBindings {
    fn memory_limit_bytes(&self) -> Option<u64> {
        self.0.memory_limit_bytes()
    }

    fn native_io(&mut self, request: Request) -> Result<Response, Error> {
        let Request::Poll(handles) = request else {
            return self.0.native_io(request);
        };
        if handles.is_empty() {
            return Err(Error::Unsupported(
                "local debugger cannot wait on an empty poll set",
            ));
        }
        let mut waited = false;
        loop {
            if self.1.load(Ordering::Acquire) {
                return Err(Error::Unsupported("local debugger session stopped"));
            }
            // A pass after a wait is a slice of its own, or a timer could
            // never become ready while the debugger holds the slice open. This
            // loosens the per-slice freeze that denies guests a clock (see
            // `HostBindings::begin_slice`), which is acceptable only because a
            // debugger session is local development, never a shared host. The
            // first pass stays in the current slice: the guest's clock moves
            // only between slices, so a timer that became ready here would be
            // one the guest finds not yet due, re-arms and polls again, forever.
            if waited {
                self.0.begin_slice();
            }
            let mut ready = Vec::new();
            for (index, handle) in handles.iter().enumerate() {
                match self.0.native_io(Request::Ready(*handle))? {
                    Response::Ready(true) => {
                        ready.push(u32::try_from(index).map_err(|_| Error::ResourceLimit)?)
                    }
                    Response::Ready(false) => {}
                    _ => {
                        return Err(Error::Unsupported(
                            "invalid local debugger readiness response",
                        ))
                    }
                }
            }
            if !ready.is_empty() {
                return Ok(Response::ReadyIndexes(ready));
            }
            // Native readiness is reactor-owned; this development-only wait keeps the slice running.
            std::thread::park_timeout(READINESS_INTERVAL);
            waited = true;
        }
    }
    fn call_future(&mut self, call: ImportCall<'_>) -> u64 {
        self.0.call_future(call)
    }
    fn call_sync(&mut self, call: ImportCall<'_>) -> i64 {
        self.0.call_sync(call)
    }
    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        output: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        self.0.call_fill(call, output)
    }
    fn slice_clock_ms(&self) -> u64 {
        self.0.slice_clock_ms()
    }
    fn begin_slice(&mut self) {
        self.0.begin_slice();
    }
    fn end_slice(&mut self) {
        self.0.end_slice();
    }
    fn is_init_sandbox(&self) -> bool {
        self.0.is_init_sandbox()
    }
    fn enter_grace(&mut self) {
        self.0.enter_grace();
    }
}

struct DebugInstance {
    instance: Instance,
    initialized: bool,
    pending_dispatch: Option<DispatchRecord>,
}

impl DebugInstance {
    fn dispatch_initialized(
        &mut self,
        record: &DispatchRecord,
        budget: OccupancyBudget,
        initialize: DispatchOutcome,
    ) -> DispatchOutcome {
        let mut result = self.instance.dispatch(
            record,
            budget.after(budget.spent_ns.saturating_add(initialize.occupancy_ns)),
        );
        result.occupancy_ns = result.occupancy_ns.saturating_add(initialize.occupancy_ns);
        result.wall_ns = result.wall_ns.saturating_add(initialize.wall_ns);
        result.grace_deferrals = result
            .grace_deferrals
            .saturating_add(initialize.grace_deferrals);
        result.occupancy_measurement = match (
            initialize.occupancy_measurement,
            result.occupancy_measurement,
        ) {
            (OccupancyMeasurement::Incomplete, _) | (_, OccupancyMeasurement::Incomplete) => {
                OccupancyMeasurement::Incomplete
            }
            (OccupancyMeasurement::NotExecuted, OccupancyMeasurement::NotExecuted) => {
                OccupancyMeasurement::NotExecuted
            }
            _ => OccupancyMeasurement::Complete,
        };
        result
    }
}

impl InstanceOps for DebugInstance {
    fn dispatch(&mut self, record: &DispatchRecord, budget: OccupancyBudget) -> DispatchOutcome {
        if self.initialized {
            return unsupported("a local debugger instance accepts one dispatch");
        }
        self.initialized = true;
        let initialize =
            tracing::info_span!("initialize").in_scope(|| self.instance.run_initialize(budget));
        if initialize.completion == Completion::Parked {
            self.pending_dispatch = Some(record.clone());
        }
        if initialize.completion != Completion::Done {
            return initialize;
        }
        self.dispatch_initialized(record, budget, initialize)
    }
    fn wake(
        &mut self,
        dispatch: u64,
        future: u64,
        result: &ResultRecord,
        budget: OccupancyBudget,
    ) -> DispatchOutcome {
        let outcome = self.instance.wake(dispatch, future, result, budget);
        if outcome.completion == Completion::Parked {
            return outcome;
        }
        match self.pending_dispatch.take() {
            Some(record) if outcome.completion == Completion::Done => {
                self.dispatch_initialized(&record, budget, outcome)
            }
            _ => outcome,
        }
    }
    fn gc_hint(&mut self, _: u8) -> DispatchOutcome {
        unsupported("local debugger maintenance execution is unsupported")
    }
    fn is_parked(&self) -> bool {
        self.instance.is_parked()
    }
    fn is_capture_eligible(&self) -> bool {
        false
    }
    fn memory(&mut self) -> &mut [u8] {
        self.instance.memory()
    }
    fn memory_len(&self) -> usize {
        self.instance.memory_len()
    }
    fn storage_failed(&self) -> bool {
        self.instance.storage_failed()
    }
    fn capture_mutable_state(&mut self) -> anyhow::Result<InstanceImage> {
        anyhow::bail!("local debugger instances cannot be captured")
    }
    fn restore_mutable_state(&mut self, _: &InstanceImage) -> anyhow::Result<()> {
        anyhow::bail!("local debugger instances cannot be restored")
    }
}

fn unsupported(message: &'static str) -> DispatchOutcome {
    DispatchOutcome {
        status: isoplane_abi::status::PollStatus::Trapped {
            code: isoplane_abi::status::TrapCode::CapabilityViolation,
        },
        completion: Completion::Trapped,
        occupancy_ns: 0,
        occupancy_measurement: OccupancyMeasurement::NotExecuted,
        execution_limit: None,
        wall_ns: 0,
        trap: Some(isoplane_engine::TrapInfo {
            kind: isoplane_engine::TrapKind::CapabilityViolation,
            message: message.into(),
        }),
        grace_end: None,
        overage_ns: 0,
        grace_deferrals: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellBindings, InstanceCtx, InstanceId};

    use isoplane_abi::dispatch::{DispatchKind, HttpRequest, TraceContext};

    fn activator(initialize: Option<&str>) -> Result<DebugActivator, DebugError> {
        activator_with_wake(initialize, "")
    }

    fn activator_with_wake(
        initialize: Option<&str>,
        wake: &str,
    ) -> Result<DebugActivator, DebugError> {
        let initialize = initialize.map_or(String::new(), |body| {
            format!("(func (export \"isoplane_initialize\") (result i64) {body} i64.const 0)")
        });
        let wasm = wat::parse_str(format!(
            r#"(module
            (memory (export "memory") 1 1)
            (global $initialized (mut i32) (i32.const 0))
            (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 1024)
            (func (export "isoplane_handle") (param i32 i32) (result i64)
                global.get $initialized i32.const 42 i32.ne if unreachable end
                i32.const 8 i32.const 7 i32.store i64.const 0)
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) {wake} i64.const 0)
            {initialize})"#
        ))
        .expect("the debug fixture must assemble");
        let engine = Arc::new(
            IsoplaneEngine::new(isoplane_engine::EngineConfig {
                max_instances: 2,
                max_memory_bytes: 65536,
                guard_size_bytes: 65536,
                ..Default::default()
            })
            .expect("the fixture engine must start"),
        );
        let template = engine
            .load_template(&wasm)
            .expect("the debug fixture must compile");
        DebugActivator::new(engine, ModuleId::new("debug", "v1"), template)
    }

    fn instance(initialize: &str) -> Box<dyn InstanceOps + Send> {
        let activator = activator(Some(initialize)).expect("the raw fixture must load");
        activate(activator)
    }

    fn activate(activator: DebugActivator) -> Box<dyn InstanceOps + Send> {
        let ctx = Arc::new(InstanceCtx::for_test(
            InstanceId(1),
            isoplane_abi::config::Limits::default(),
        ));
        ctx.begin_dispatch(1, 1);
        activator
            .activate(CellBindings::boxed(ctx))
            .expect("the raw fixture must activate")
    }

    fn record() -> DispatchRecord {
        DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "GET".into(),
                url: "http://debug.test/".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            occupancy_budget_ms: DEBUG_BUDGET_MS,
            pending_futures_limit: 64,
            now_ms: 1,
        }
    }

    #[test]
    fn raw_initialization_runs_before_the_first_dispatch() {
        let mut instance = instance("i32.const 42 global.set $initialized");
        let result = instance.dispatch(&record(), OccupancyBudget::default());
        assert_eq!(
            result.completion,
            Completion::Done,
            "the dispatch must observe raw initialization"
        );
        assert_eq!(
            &instance.memory()[8..12],
            &7i32.to_le_bytes(),
            "the initialized dispatch must run"
        );
    }

    #[test]
    fn initialization_failure_prevents_dispatch_execution() {
        let mut instance = instance("unreachable");
        let result = instance.dispatch(&record(), OccupancyBudget::default());
        assert_eq!(
            result.completion,
            Completion::Trapped,
            "initialization failure must end the dispatch"
        );
        assert!(
            result.trap.is_some(),
            "initialization failure must remain observable"
        );
    }

    #[test]
    fn asynchronous_initialization_runs_the_saved_dispatch_after_its_final_wake() {
        let mut instance = activate(
            activator_with_wake(
                Some("i64.const 1 return"),
                "global.get $initialized i32.const 1 i32.add global.set $initialized
                 global.get $initialized i32.const 2 i32.lt_u if i64.const 1 return end
                 i32.const 42 global.set $initialized",
            )
            .expect("the asynchronous initialization fixture must load"),
        );
        assert_eq!(
            instance
                .dispatch(&record(), OccupancyBudget::default())
                .completion,
            Completion::Parked,
            "asynchronous initialization must park the original dispatch"
        );
        assert_eq!(
            instance
                .wake(
                    1,
                    1,
                    &ResultRecord::Ok(Vec::new()),
                    OccupancyBudget::default()
                )
                .completion,
            Completion::Parked,
            "an intermediate initialization wake must keep the dispatch pending"
        );
        assert_eq!(
            &instance.memory()[8..12],
            &0i32.to_le_bytes(),
            "the dispatch cannot run before initialization finishes"
        );
        assert_eq!(
            instance
                .wake(
                    1,
                    2,
                    &ResultRecord::Ok(Vec::new()),
                    OccupancyBudget::default()
                )
                .completion,
            Completion::Done,
            "the final initialization wake must run the original dispatch"
        );
        assert_eq!(
            &instance.memory()[8..12],
            &7i32.to_le_bytes(),
            "completion must include the queued handler, not just initialization"
        );
    }

    #[test]
    fn a_failed_initialization_wake_never_runs_the_saved_dispatch() {
        let mut instance = activate(
            activator_with_wake(Some("i64.const 1 return"), "unreachable")
                .expect("the failing asynchronous initialization fixture must load"),
        );
        assert_eq!(
            instance
                .dispatch(&record(), OccupancyBudget::default())
                .completion,
            Completion::Parked,
            "the fixture must reach its asynchronous initialization boundary"
        );
        assert_eq!(
            instance
                .wake(
                    1,
                    1,
                    &ResultRecord::Ok(Vec::new()),
                    OccupancyBudget::default()
                )
                .completion,
            Completion::Trapped,
            "a failed initialization wake must end the dispatch"
        );
        assert_eq!(
            &instance.memory()[8..12],
            &0i32.to_le_bytes(),
            "failed initialization must not invoke the queued handler"
        );
    }

    #[test]
    fn debugger_instances_refuse_capture_and_a_second_dispatch() {
        let mut instance = instance("i32.const 42 global.set $initialized");
        let result = instance.dispatch(&record(), OccupancyBudget::default());
        assert_eq!(
            result.completion,
            Completion::Done,
            "the first dispatch must finish"
        );
        assert!(
            !instance.is_capture_eligible(),
            "debugger state cannot become a checkpoint"
        );
        assert!(
            instance.capture_mutable_state().is_err(),
            "direct capture must also refuse debugger state"
        );
        assert_eq!(
            instance
                .dispatch(&record(), OccupancyBudget::default())
                .completion,
            Completion::Trapped,
            "a debugger instance cannot be reused for another dispatch"
        );
    }

    #[test]
    fn debugger_instances_refuse_restoration_and_maintenance_execution() {
        let mut instance = instance("i32.const 42 global.set $initialized");
        assert!(
            instance
                .restore_mutable_state(&InstanceImage::default())
                .is_err(),
            "debugger transport state cannot be restored from another session"
        );
        assert_eq!(
            instance.gc_hint(255).completion,
            Completion::Trapped,
            "debugger maintenance callbacks must be refused explicitly"
        );
    }

    #[test]
    fn raw_debug_loading_requires_an_initialization_export() {
        assert!(
            matches!(activator(None), Err(DebugError::MissingInitialize)),
            "a prepared artifact cannot stand in for raw debug initialization"
        );
    }

    #[test]
    fn debug_bindings_preserve_the_application_memory_ceiling() {
        let context = Arc::new(crate::bindings::InstanceCtx::for_test(
            crate::InstanceId(1),
            isoplane_abi::config::Limits::default(),
        ));
        let bindings = DebugBindings(
            Box::new(crate::bindings::CellBindings::new(context)),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(
            bindings.memory_limit_bytes(),
            Some(isoplane_abi::config::Limits::default().instance_memory_max),
            "debugging must retain the application's memory limit"
        );
    }

    struct Readiness {
        calls: usize,
        malformed: bool,
    }
    impl HostBindings for Readiness {
        fn begin_slice(&mut self) {}
        fn end_slice(&mut self) {}
        fn native_io(&mut self, request: Request) -> Result<Response, Error> {
            assert!(
                matches!(request, Request::Ready(_)),
                "debug waiting must use existing readiness capabilities"
            );
            self.calls += 1;
            if self.malformed {
                return Ok(Response::Handle(isoplane_abi::native_io::ResourceHandle(1)));
            }
            Ok(Response::Ready(self.calls > 1))
        }
        fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
            panic!("unexpected future")
        }
        fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
            panic!("unexpected sync")
        }
        fn slice_clock_ms(&self) -> u64 {
            0
        }
    }

    #[test]
    fn local_poll_waits_until_the_host_reports_readiness() {
        let mut bindings = DebugBindings(
            Box::new(Readiness {
                calls: 0,
                malformed: false,
            }),
            Arc::new(AtomicBool::new(false)),
        );
        let result = bindings.native_io(Request::Poll(vec![
            isoplane_abi::native_io::ResourceHandle(1),
        ]));
        assert!(
            matches!(result, Ok(Response::ReadyIndexes(indexes)) if indexes == vec![0]),
            "local waiting must return the index of a host-ready resource"
        );
    }

    #[test]
    fn stopping_a_debug_session_ends_its_readiness_wait() {
        let mut bindings = DebugBindings(
            Box::new(Readiness {
                calls: 0,
                malformed: false,
            }),
            Arc::new(AtomicBool::new(true)),
        );
        let result = bindings.native_io(Request::Poll(vec![
            isoplane_abi::native_io::ResourceHandle(1),
        ]));
        assert!(
            matches!(
                result,
                Err(Error::Unsupported("local debugger session stopped"))
            ),
            "a stopped debugger must release its native readiness wait"
        );
    }

    #[test]
    fn an_empty_debug_poll_refuses_instead_of_waiting_forever() {
        let mut bindings = DebugBindings(
            Box::new(Readiness {
                calls: 0,
                malformed: false,
            }),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(
                bindings.native_io(Request::Poll(vec![])),
                Err(Error::Unsupported(_))
            ),
            "a local debugger needs at least one wake source"
        );
    }

    #[test]
    fn malformed_host_readiness_is_refused_instead_of_treated_as_ready() {
        let mut bindings = DebugBindings(
            Box::new(Readiness {
                calls: 0,
                malformed: true,
            }),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(
                bindings.native_io(Request::Poll(vec![
                    isoplane_abi::native_io::ResourceHandle(1)
                ])),
                Err(Error::Unsupported(_))
            ),
            "a malformed readiness reply must fail the local debugger wait"
        );
    }
}
