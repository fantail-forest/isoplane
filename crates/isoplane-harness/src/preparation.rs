//! Templates prepared the way a node and the command line prepare them: the
//! module's runtime stage as a baseline, then its application stage as a
//! layer over that baseline, each as its stage's preparation loaded it.

use std::sync::Arc;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::snapshot::GlobalValue;
use isoplane_engine::{
    Completion, EngineConfig, Instance, InstanceOps, IsoplaneEngine, OccupancyBudget, Template,
};
use isoplane_prepare::{
    BuildProduct, BuildRequest, Initialization, Initialized, LocalPreparation, PrepareError,
    RuntimeBaseline, RuntimeProfile,
};

/// A prepared template.
pub struct Prepared {
    /// The template, with the initializer's memory and globals.
    pub template: Template,
    /// What the application's initialization reported.
    pub initialized: Initialized,
    /// The engine `template` is loaded in.
    pub engine: Arc<IsoplaneEngine>,
}

impl Prepared {
    /// A copy of the template's whole linear memory.
    pub fn memory(&self) -> Vec<u8> {
        self.template.memory_image_arc().to_dense()
    }

    /// The values an instance of the template starts with in its mutable
    /// globals, one per export of a global, in the module's global order.
    /// Preparation exports every mutable global of a module with an
    /// initializer, so a global the module also exports itself appears twice.
    pub fn globals(&self) -> Vec<GlobalValue> {
        activate(&self.engine, &self.template)
            .capture_mutable_state()
            .expect("the instance's globals must be readable")
            .globals
    }

    /// The memory of a new instance of the template after one background
    /// dispatch its `isoplane_handle` completed: what the guest itself reads
    /// of the state it starts with.
    pub fn memory_after_dispatch(&self) -> Vec<u8> {
        let mut instance = activate(&self.engine, &self.template);
        assert_eq!(
            dispatch(&mut instance),
            Completion::Done,
            "the guest must complete the dispatch"
        );
        instance.memory().to_vec()
    }
}

/// A new instance of `template` in `engine`, which is answered no host call.
pub fn activate(engine: &IsoplaneEngine, template: &Template) -> Instance {
    engine
        .activate_initialization(template, Box::new(NoImports))
        .expect("the template must activate")
}

/// Runs one background dispatch in `instance` and returns how it completed.
pub fn dispatch(instance: &mut Instance) -> Completion {
    let record = isoplane_abi::dispatch::DispatchRecord {
        dispatch_id: 1,
        kind: isoplane_abi::dispatch::DispatchKind::Background,
        cancel_future: 0,
        occupancy_budget_ms: 1000,
        pending_futures_limit: 64,
        now_ms: 0,
    };
    instance
        .dispatch(&record, OccupancyBudget::default())
        .completion
}

struct NoImports;

impl isoplane_engine::bindings::InitializationBindings for NoImports {
    fn begin_slice(&mut self) {}
    fn call_sync(
        &mut self,
        call: isoplane_engine::ImportCall<'_>,
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        Err(isoplane_engine::bindings::BindingError::Unsupported(
            call.import,
        ))
    }
    fn call_fill(
        &mut self,
        call: isoplane_engine::ImportCall<'_>,
        _output: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        Err(isoplane_engine::bindings::BindingError::Unsupported(
            call.import,
        ))
    }
}

/// The request for `wasm` as an application on a shared runtime, with the
/// default memory limit and no application files.
pub fn request(wasm: &[u8], engine: EngineConfig, initialization: Initialization) -> BuildRequest {
    BuildRequest {
        engine,
        application_files: Default::default(),
        runtime: RuntimeProfile::SharedRuntime {
            sdk_version: "test".into(),
        },
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization,
        module: isoplane_prepare::Module::new(wasm),
    }
}

/// Prepares the module `request` carries as it describes: its runtime stage
/// as a baseline and its application stage as a layer over it. Returns what
/// either stage's preparation refused; [`baseline_refusal`] and
/// [`application_refusal`] say which.
pub fn prepare(request: &BuildRequest) -> Result<Prepared, PrepareError> {
    application(&baseline(request)?, request)
}

/// What preparing the runtime stage of `request`'s module as a baseline
/// refused.
pub fn baseline_refusal(request: &BuildRequest) -> PrepareError {
    match baseline(request) {
        Ok(_) => panic!("the runtime stage must be refused as a baseline"),
        Err(error) => error,
    }
}

/// What preparing the application stage of `request`'s module refused, over
/// the baseline its runtime stage prepared as.
pub fn application_refusal(request: &BuildRequest) -> PrepareError {
    let baseline = baseline(request).unwrap_or_else(|error| {
        panic!("the runtime stage must prepare as a baseline before the application stage can be refused: {error}")
    });
    match application(&baseline, request) {
        Ok(_) => panic!("the application stage must be refused"),
        Err(error) => error,
    }
}

fn baseline(request: &BuildRequest) -> Result<RuntimeBaseline, PrepareError> {
    Ok(RuntimeBaseline::prepare(&request.baseline(), None)?.baseline)
}

fn application(
    baseline: &RuntimeBaseline,
    request: &BuildRequest,
) -> Result<Prepared, PrepareError> {
    let captured = tempfile::tempdir().expect("a directory for captured state");
    let application = baseline.prepare_application_in(request, captured.path())?;
    Ok(Prepared {
        template: application.template,
        initialized: application.product.initialized,
        engine: Arc::clone(baseline.engine()),
    })
}

/// Publishes `product`, prepared as `identity`, in a new private directory
/// and loads it into `engine`, over `base` when it is a layer. The template
/// keeps the directory until it and its instances are released.
pub fn load(
    engine: &IsoplaneEngine,
    product: &BuildProduct,
    identity: PreparedIdentity,
    base: Option<&Template>,
) -> Template {
    let publication = tempfile::tempdir().expect("a publication directory");
    // SAFETY: `publication` is new, and its path goes nowhere but to this
    // call, so nothing changes the files published there. The template keeps
    // the directory for as long as it maps them.
    let mut template = unsafe {
        load_in(
            engine,
            product,
            identity,
            base,
            &publication.path().join("published"),
        )
    };
    template.retain_storage(Arc::new(publication));
    template
}

/// As [`load`], publishing in `directory`, for a test of what is published.
///
/// # Safety
/// Nothing may change the files in `directory` while the returned template,
/// a template loaded over it or an instance of either exists: the template
/// maps them and runs the native code they hold.
pub unsafe fn load_in(
    engine: &IsoplaneEngine,
    product: &BuildProduct,
    identity: PreparedIdentity,
    base: Option<&Template>,
    directory: &std::path::Path,
) -> Template {
    let bytes = product.prepared_bytes();
    // SAFETY: preparation in this process produced these bytes.
    let prepared = unsafe {
        match base {
            Some(base) => engine.decode_builder_output_over(base, bytes, identity),
            None => engine.decode_builder_output(bytes, identity),
        }
    }
    .expect("the prepared output must decode");
    prepared
        .publish(directory)
        .expect("the prepared output must publish");
    // SAFETY: preparation in this process produced the decoded native code,
    // and this function's caller keeps the files in `directory` unchanged.
    unsafe {
        match base {
            Some(base) => engine.load_prepared_over(base, directory, identity, &[]),
            None => engine.load_prepared(directory, identity, &[]),
        }
    }
    .expect("the prepared output must load")
}

/// A local preparation provider and the directory holding its compilation cache.
pub struct LocalProvider {
    preparation: LocalPreparation,
    _cache: tempfile::TempDir,
}

impl std::ops::Deref for LocalProvider {
    type Target = LocalPreparation;

    fn deref(&self) -> &LocalPreparation {
        &self.preparation
    }
}

/// A local preparation provider with an empty compilation cache of its own.
pub fn local_preparation() -> LocalProvider {
    let directory = tempfile::tempdir().expect("a compilation cache directory");
    // SAFETY: only the cache's compiler writes into this new, private directory.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(directory.path().join("cache"))
    }
    .expect("the compilation cache must open");
    LocalProvider {
        preparation: LocalPreparation::with_cache(Arc::new(cache))
            .expect("the local provider must start"),
        _cache: directory,
    }
}

/// Prepares the module `request` carries, one that carries its whole
/// application as a guest built with the Rust SDK does, the way a node
/// prepares one: its whole initialization as one layer.
pub async fn prepare_whole_module(request: &BuildRequest) -> Result<Prepared, PrepareError> {
    use isoplane_prepare::Preparation;
    let product = local_preparation()
        .prepare(request, &tokio_util::sync::CancellationToken::new())
        .await?;
    let engine = Arc::new(
        IsoplaneEngine::new(request.engine.clone()).expect("the request's engine must start"),
    );
    let template = load(
        &engine,
        &product,
        request.identity().expect("the module's identity"),
        None,
    );
    Ok(Prepared {
        template,
        initialized: product.initialized,
        engine,
    })
}

/// A guest whose runtime stage runs `runtime` once, through either
/// `isoplane_initialize_runtime` or the start of `isoplane_initialize`, which
/// then runs `init`: the contract the SDK's generated exports keep.
pub fn staged_guest(imports: &str, runtime: &str, init: &str) -> Vec<u8> {
    staged_guest_with_handle(imports, runtime, init, "(i64.const 0)")
}

/// As [`staged_guest`], with `handle` as the body of `isoplane_handle`.
pub fn staged_guest_with_handle(imports: &str, runtime: &str, init: &str, handle: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {imports}
      (memory (export "memory") 2 64)
      (global $runtime_booted (export "runtime_booted") (mut i32) (i32.const 0))
      (global $state (export "state") (mut i32) (i32.const 1))
      (func $boot_runtime
        (if (i32.eqz (global.get $runtime_booted))
          (then {runtime} (global.set $runtime_booted (i32.const 1)))))
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) {handle})
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_initialize_runtime") (result i64) (call $boot_runtime) i64.const 0)
      (func (export "isoplane_initialize") (result i64) (call $boot_runtime) {init} i64.const 0))"#
    ))
    .expect("the staged test guest must assemble")
}
