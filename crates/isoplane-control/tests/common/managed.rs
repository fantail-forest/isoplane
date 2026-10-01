use isoplane_abi::config::{InitSandbox, Limits};
use isoplane_engine::{EngineConfig, PlatformFile, PlatformFiles};
use isoplane_prepare::{
    BuildProduct, BuildRequest, Initialization, PortableTemplate, RuntimeProfile, WorkerFailure,
};

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

pub fn prepare(name: &str, platform_files: PlatformFiles) -> Result<BuildProduct, WorkerFailure> {
    let wasm = isoplane_fixtures::mono_runtime();
    let request = BuildRequest {
        engine: EngineConfig {
            pooling: false,
            max_memory_bytes: 256 << 20,
            platform_files,
            ..Default::default()
        },
        application_files: files(name),
        runtime: RuntimeProfile::SharedRuntime {
            sdk_version: isoplane_fixtures::release_version(),
        },
        limits: Limits::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
        portable_template: PortableTemplate::Returned,
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
    isoplane_prepare::prepare_in_current_process(&wasm, &request)
}
