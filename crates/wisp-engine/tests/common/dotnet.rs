//! The harness for the C# behaviour fixtures: one engine and template per
//! fixture and runtime, activation with recording bindings, and the HTTP
//! dispatch the probes answer. A behaviour a customer sees is proven in both
//! runtimes, so `in_both_runtimes!` writes one test per runtime from one body
//! and a failure names the runtime it happened in.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::bindings::{Recorder, TestBindings};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_engine::{
    Completion, CpuBudget, DispatchOutcome, EngineConfig, Instance, InstanceOps, Template,
    WispEngine,
};
pub use wisp_fixtures::GuestRuntime;

/// Writes `mod $name { fn native_aot() {..} fn mono() {..} }` from one body,
/// so `cargo test mono` or `cargo test hashing::` select what to run.
#[macro_export]
macro_rules! in_both_runtimes {
    ($(fn $name:ident($runtime:ident: GuestRuntime) $body:block)*) => {
        $(
            mod $name {
                use super::*;

                #[test]
                fn native_aot() {
                    let $runtime = GuestRuntime::NativeAot;
                    $body
                }

                #[test]
                fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )*
    };
}

/// A loaded fixture: the engine that compiled it and its template.
pub struct Fixture {
    pub engine: WispEngine,
    pub template: Template,
}

/// The fixture `name` published for `runtime`, loaded once per test binary.
/// Building or loading it fails the calling test with the runtime named.
pub fn fixture(name: &'static str, runtime: GuestRuntime) -> &'static Fixture {
    static FIXTURES: OnceLock<Mutex<HashMap<(&'static str, GuestRuntime), &'static Fixture>>> =
        OnceLock::new();
    let mut fixtures = FIXTURES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    fixtures.entry((name, runtime)).or_insert_with(|| {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 16,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .unwrap_or_else(|error| panic!("the {name} fixture engine must initialize: {error:#}"));
        let template = engine
            .load_template(&wisp_fixtures::dotnet_for(name, runtime))
            .unwrap_or_else(|error| {
                panic!("the {name} fixture must load under {runtime}: {error:#}")
            });
        Box::leak(Box::new(Fixture { engine, template }))
    })
}

impl Fixture {
    /// An instance with recording bindings whose entropy starts at `first_byte`.
    pub fn activate_with_entropy(&self, first_byte: u8) -> (Instance, Recorder) {
        let recorder = Recorder::default();
        let mut bindings = TestBindings::new(recorder.clone());
        bindings.fill_byte = first_byte;
        let instance = self
            .engine
            .activate(&self.template, Box::new(bindings))
            .expect("the fixture must activate");
        (instance, recorder)
    }

    /// An instance with recording bindings and the default entropy pattern.
    pub fn activate(&self) -> (Instance, Recorder) {
        self.activate_with_entropy(TestBindings::new(Recorder::default()).fill_byte)
    }

    /// A second activation that resumes the state captured from `original`,
    /// the way a wake after a Store replacement does. `original` is consumed
    /// because its memory has been copied out.
    pub fn restore_from(&self, mut original: Instance, first_byte: u8) -> (Instance, Recorder) {
        let image = original
            .capture_mutable_state()
            .expect("a returned managed guest must be capturable");
        let memory = original.memory().to_vec();
        drop(original);
        let (mut restored, recorder) = self.activate_with_entropy(first_byte);
        restored
            .restore_mutable_state(&image)
            .expect("captured managed globals must restore");
        restored.memory().copy_from_slice(&memory);
        (restored, recorder)
    }
}

/// A generous budget: these tests prove behaviour, not limits.
pub fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

/// An HTTP dispatch a probe answers, built from the parts a test varies.
pub struct Request {
    method: &'static str,
    path: String,
    query: Vec<(String, String)>,
    body: Vec<u8>,
    now_ms: u64,
}

impl Request {
    pub fn get(path: &str) -> Self {
        Self::new("GET", path)
    }

    pub fn post(path: &str) -> Self {
        Self::new("POST", path)
    }

    fn new(method: &'static str, path: &str) -> Self {
        Self {
            method,
            path: path.to_string(),
            query: Vec::new(),
            body: Vec::new(),
            now_ms: 1,
        }
    }

    pub fn query(mut self, key: &str, value: impl ToString) -> Self {
        self.query.push((key.to_string(), value.to_string()));
        self
    }

    pub fn body(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }

    pub fn now_ms(mut self, now_ms: u64) -> Self {
        self.now_ms = now_ms;
        self
    }

    pub fn record(&self) -> DispatchRecord {
        DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: self.method.into(),
                url: format!("https://probe.test{}", self.path),
                path: self.path.clone(),
                query: self.query.clone(),
                headers: vec![],
                body: self.body.clone(),
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: budget().cpu_ms,
            now_ms: self.now_ms,
        }
    }

    /// Dispatches and returns the outcome, whatever it was.
    pub fn dispatch(&self, instance: &mut Instance) -> DispatchOutcome {
        instance.dispatch(&self.record(), budget())
    }

    /// Dispatches a request the probe must answer synchronously with 200 and
    /// returns the body it wrote.
    pub fn answer(&self, instance: &mut Instance, recorder: &Recorder) -> Vec<u8> {
        let outcome = self.dispatch(instance);
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "the probe must finish {} synchronously: {outcome:?}; imports: {:?}",
            self.path,
            recorder.calls.lock()
        );
        let response = recorder
            .responses
            .lock()
            .pop()
            .unwrap_or_else(|| panic!("the probe must respond to {}", self.path));
        assert_eq!(
            response.status, 200,
            "the probe must report success for {}",
            self.path
        );
        response.body
    }
}
