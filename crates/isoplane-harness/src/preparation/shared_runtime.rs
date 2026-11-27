//! Applications prepared on the shared .NET runtime the way a node and the
//! command line prepare them: the runtime once, as a baseline, and each
//! application as a layer over it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use isoplane_engine::prepared::PreparedTemplate;
use isoplane_engine::{CompilationCache, IsoplaneEngine, PlatformFiles, Template};
use isoplane_prepare::{
    BaselineRequest, BuildRequest, Initialization, Initialized, Module, PrepareError,
    RuntimeBaseline, RuntimeProfile,
};

/// An application prepared as a layer over the shared runtime's baseline,
/// with that baseline. Clones share both.
#[derive(Clone)]
pub struct Application {
    /// What the application's initialization reported.
    pub initialized: Arc<Initialized>,
    baseline: Baseline,
    layer: Arc<PreparedTemplate>,
}

impl Application {
    /// What the runtime's own initialization reported when its baseline was
    /// prepared.
    pub fn runtime_initialized(&self) -> &Initialized {
        self.baseline.initialized()
    }

    /// The application's template in `engine`, as a node loads it: the
    /// baseline, then the application's layer over it.
    pub fn load(&self, engine: &IsoplaneEngine) -> Template {
        let storage = std::env::temp_dir();
        let base = self.baseline.load(engine);
        engine
            .load_prepared_product_over_in(&base, &self.layer, &storage)
            .unwrap_or_else(|error| {
                panic!("the application must load over its baseline: {error:#}")
            })
    }
}

/// The shared runtime Task built, prepared as a baseline. Clones share it.
#[derive(Clone)]
pub struct Baseline(Arc<PreparedBaseline>);

struct PreparedBaseline {
    product: PreparedTemplate,
    initialized: Initialized,
}

impl Baseline {
    /// Prepares the baseline `request` derives, compiling the runtime
    /// through `cache`. Panics if the runtime does not prepare.
    pub fn prepare(request: &BaselineRequest, cache: &CompilationCache) -> Self {
        on_its_own_thread(|| {
            let prepared = RuntimeBaseline::prepare(request, cache).unwrap_or_else(|error| {
                panic!("the shared runtime must prepare as a baseline: {error:?}")
            });
            let identity = request.identity().expect("the baseline's identity");
            // SAFETY: `RuntimeBaseline::prepare` in this process produced these bytes.
            let product = unsafe {
                prepared
                    .baseline
                    .engine()
                    .decode_builder_output(prepared.product.prepared_bytes(), identity)
            }
            .expect("the runtime baseline must decode");
            Self(Arc::new(PreparedBaseline {
                product,
                initialized: prepared.product.initialized,
            }))
        })
    }

    /// What the runtime's own initialization reported.
    pub fn initialized(&self) -> &Initialized {
        &self.0.initialized
    }

    /// Prepares `request`'s application as a layer over this baseline.
    /// Returns the error from preparing the layer; a baseline that does not
    /// load for `request`'s engine panics.
    pub fn prepare_application(&self, request: &BuildRequest) -> Result<Application, PrepareError> {
        on_its_own_thread(|| {
            let engine = Arc::new(
                IsoplaneEngine::new(request.engine.clone())
                    .expect("the request's engine must start"),
            );
            let runtime = RuntimeBaseline::loaded(
                Arc::clone(&engine),
                self.load(&engine),
                &request.baseline(),
            )
            .expect("the runtime baseline must still run the application's initializer");
            let captured = tempfile::tempdir().expect("a directory for captured state");
            let application = runtime.prepare_application_in(request, captured.path())?;
            let identity = request.identity().expect("the application's identity");
            // SAFETY: `prepare_application_in` in this process produced these bytes.
            let layer = unsafe {
                engine.decode_builder_output_over(
                    runtime.template(),
                    application.product.prepared_bytes(),
                    identity,
                )
            }
            .expect("the application's layer must decode over its baseline");
            Ok(Application {
                initialized: Arc::new(application.product.initialized),
                baseline: self.clone(),
                layer: Arc::new(layer),
            })
        })
    }

    fn load(&self, engine: &IsoplaneEngine) -> Template {
        engine
            .load_prepared_product_in(&self.0.product, &std::env::temp_dir())
            .unwrap_or_else(|error| panic!("the runtime baseline must load: {error:#}"))
    }
}

fn runtime() -> &'static Module {
    static RUNTIME: OnceLock<Module> = OnceLock::new();
    RUNTIME.get_or_init(|| Module::new(&isoplane_fixtures::mono_runtime()))
}

/// The request for `application_files` as an application on the shared
/// runtime Task built, for a node serving `platform_files` on the engine
/// [`crate::engine_configuration`] describes: the default limits and
/// initialization budgets, no settings and no network.
pub fn request(application_files: PlatformFiles, platform_files: PlatformFiles) -> BuildRequest {
    BuildRequest {
        engine: crate::engine_configuration(platform_files),
        application_files,
        runtime: RuntimeProfile::SharedRuntime {
            sdk_version: isoplane_fixtures::release_version(),
        },
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: Initialization::new(b"{}".to_vec()),
        module: runtime().clone(),
    }
}

/// Prepares `request`'s application as a layer over the shared runtime's
/// baseline, which every request deriving the same one shares. Returns the
/// error from preparing the layer; a fixture that cannot be set up panics.
pub fn prepare(request: &BuildRequest) -> Result<Application, PrepareError> {
    shared_baseline(request).prepare_application(request)
}

/// The baseline `request` derives, prepared by the first caller that wants
/// it from the compilation Task made of the runtime.
fn shared_baseline(request: &BuildRequest) -> Baseline {
    type Key = ([u8; 32], [u8; 32], [u8; 32]);
    type Baselines = parking_lot::Mutex<HashMap<Key, Arc<OnceLock<Baseline>>>>;
    static BASELINES: OnceLock<Baselines> = OnceLock::new();
    let wanted = request.baseline();
    let identity = wanted.identity().expect("the baseline's identity");
    let prepared = BASELINES
        .get_or_init(Default::default)
        .lock()
        .entry((
            identity.input_hash,
            identity.runtime_profile,
            identity.initialization_hash,
        ))
        .or_default()
        .clone();
    prepared
        .get_or_init(|| Baseline::prepare(&wanted, &isoplane_fixtures::compiled_runtime_cache()))
        .clone()
}

/// Preparation starts a runtime of its own to drive initialization's network,
/// which a thread already inside a test's runtime may not do.
fn on_its_own_thread<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(work)
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
