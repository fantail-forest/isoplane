use isoplane_engine::{EngineConfig, IsoplaneEngine};
use isoplane_prepare::Initialization;

pub fn engine_config() -> EngineConfig {
    EngineConfig {
        max_instances: 4,
        // The ceiling of a 4 MiB memory limit.
        max_memory_bytes: isoplane_abi::config::memory_ceiling_bytes(4 << 20),
        guard_size_bytes: 2 << 20,
        ..EngineConfig::default()
    }
}

#[allow(
    dead_code,
    reason = "the platform-file tests build requests from the configuration alone"
)]
pub fn engine() -> IsoplaneEngine {
    IsoplaneEngine::new(engine_config()).expect("the preparation engine must initialize")
}

pub fn request() -> Initialization {
    Initialization {
        network: None,
        runtime_config: b"{}".to_vec(),
        occupancy_ms: 1000,
        wall_ms: 5000,
        log_bytes: 1024,
    }
}
