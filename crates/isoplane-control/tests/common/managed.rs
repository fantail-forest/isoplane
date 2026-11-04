use isoplane_abi::config::InitSandbox;
use isoplane_engine::{EngineConfig, PlatformFile, PlatformFiles};
use isoplane_prepare::Preparation as _;
use isoplane_prepare::{BuildProduct, BuildRequest, Initialization, RuntimeProfile, WorkerFailure};

pub fn files(name: &str) -> PlatformFiles {
    PlatformFiles::new(
        isoplane_fixtures::managed_probe(name)
            .into_iter()
            .map(|file| PlatformFile {
                path: file.path,
                bytes: file.bytes.into(),
                modified_ns: 0,
            })
            .collect(),
    )
    .expect("valid managed application files")
}

pub struct Prepared {
    pub product: BuildProduct,
    configuration: EngineConfig,
    identity: isoplane_engine::prepared::PreparedIdentity,
}

impl Prepared {
    pub fn artifact(&self) -> isoplane_engine::prepared::PreparedTemplate {
        let engine = isoplane_engine::IsoplaneEngine::new(self.configuration.clone()).unwrap();
        // SAFETY: only the local preparation provider in prepare constructs this record's product.
        unsafe { engine.decode_builder_output(self.product.prepared_bytes(), self.identity) }
            .expect("trusted prepared application")
    }
}

pub fn prepare(name: &str, platform_files: PlatformFiles) -> Result<Prepared, WorkerFailure> {
    let wasm = isoplane_fixtures::mono_runtime();
    let request = BuildRequest {
        engine: EngineConfig {
            platform_files,
            ..isoplane_fixtures::engine_config(2)
        },
        application_files: files(name),
        runtime: RuntimeProfile::SharedRuntime {
            sdk_version: isoplane_fixtures::release_version(),
        },
        memory_limit: isoplane_abi::config::GuestMemoryBytes::new(
            isoplane_abi::config::DEFAULT_MEMORY_LIMIT_BYTES,
        )
        .expect("the fixture engine memory limit"),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
        initialization: Initialization {
            network: None,

            config: Vec::new(),
            runtime_config: b"{}".to_vec(),
            sandbox: InitSandbox::RuntimeBringUp,
            occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        },
    };
    let preparation =
        isoplane_prepare::LocalPreparation::with_cache(isoplane_fixtures::compiled_runtime_cache())
            .expect("local preparation");
    // Its own thread, so the async tests that call this can wait for it too.
    let prepared = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("preparation runtime")
                    .block_on(preparation.prepare(
                        &wasm,
                        &request,
                        &tokio_util::sync::CancellationToken::new(),
                    ))
            })
            .join()
            .expect("preparation does not panic")
    });
    match prepared {
        Ok(product) => Ok(Prepared {
            product,
            configuration: request.engine.clone(),
            identity: request.identity().unwrap(),
        }),
        Err(isoplane_prepare::PrepareError::Rejected(failure)) => Err(failure),
        Err(error) => panic!("preparation must run to an outcome: {error}"),
    }
}
