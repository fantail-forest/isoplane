use std::sync::Arc;
use std::time::Duration;
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_abi::result::ResultRecord;
use wisp_engine::{
    Completion, CpuBudget, DispatchOutcome, EngineConfig, Instance, InstanceImage, InstanceOps,
    Template, WispEngine,
};
use wisp_host::bindings::{CellBindings, InstanceCtx};

pub struct ReturnedGuest {
    engine: WispEngine,
    template: Template,
    context: Arc<InstanceCtx>,
    snapshot: Option<(InstanceImage, Vec<u8>)>,
}

impl ReturnedGuest {
    pub fn new(fixture: &str, context: Arc<InstanceCtx>) -> Self {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 2,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the native I/O fixture engine must initialize");
        let template = engine
            .load_template(&wisp_fixtures::dotnet(fixture))
            .expect("the native I/O fixture must load");
        Self {
            engine,
            template,
            context,
            snapshot: None,
        }
    }

    pub fn start(&mut self, path: &str) -> DispatchOutcome {
        let mut instance = self
            .engine
            .activate(&self.template, CellBindings::boxed(self.context.clone()))
            .expect("the native I/O fixture must activate");
        if let Some((image, memory)) = self.snapshot.take() {
            instance
                .restore_mutable_state(&image)
                .expect("the returning guest must restore its saved state");
            instance.memory().copy_from_slice(&memory);
        }
        let outcome = instance.dispatch(
            &DispatchRecord {
                dispatch_id: self.context.dispatch_id(),
                kind: DispatchKind::Http(HttpRequest {
                    method: "GET".into(),
                    url: format!("https://native.test{path}"),
                    path: path.into(),
                    query: vec![],
                    headers: vec![],
                    body: vec![],
                }),
                trace: TraceContext::default(),
                cancel_future: 0,
                cpu_budget_ms: 30_000,
                now_ms: self.context.slice_clock_ms(),
            },
            budget(),
        );
        self.snapshot = capture(&mut instance, &outcome);
        outcome
    }

    pub async fn wake(&mut self, notification: &tokio::sync::Notify) -> DispatchOutcome {
        tokio::time::timeout(Duration::from_secs(60), notification.notified())
            .await
            .expect("native I/O must signal readiness before the test deadline");
        let (future, result) = self
            .context
            .rt
            .as_ref()
            .expect("native I/O requires a reactor")
            .futures
            .take_ready(self.context.instance)
            .expect("a host wake must carry its owner's completion");
        self.restore_and_wake(future, &result)
    }

    fn restore_and_wake(&mut self, future: u64, result: &ResultRecord) -> DispatchOutcome {
        let (image, memory) = self
            .snapshot
            .take()
            .expect("a wake must follow an empty-stack park");
        let (outcome, snapshot) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut instance = self
                        .engine
                        .activate(&self.template, CellBindings::boxed(self.context.clone()))
                        .expect("the I/O guest must activate on another worker");
                    instance
                        .restore_mutable_state(&image)
                        .expect("the I/O globals must restore");
                    instance.memory().copy_from_slice(&memory);
                    let outcome = instance.wake(future, result, budget());
                    let snapshot = capture(&mut instance, &outcome);
                    (outcome, snapshot)
                })
                .join()
                .expect("the I/O worker must not panic")
        });
        self.snapshot = snapshot;
        outcome
    }
}

fn capture(instance: &mut Instance, outcome: &DispatchOutcome) -> Option<(InstanceImage, Vec<u8>)> {
    matches!(outcome.completion, Completion::Parked | Completion::Done).then(|| {
        (
            instance
                .capture_mutable_state()
                .expect("an empty-stack I/O return must be capturable"),
            instance.memory().to_vec(),
        )
    })
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}
