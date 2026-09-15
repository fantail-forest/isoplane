use wisp_abi::config::InitSandbox;
use wisp_engine::{EngineConfig, WispEngine};
use wisp_prepare::Initialization;

pub fn engine() -> WispEngine {
    WispEngine::new(EngineConfig {
        max_instances: 4,
        max_memory_bytes: 4 << 20,
        guard_size_bytes: 2 << 20,
        ..EngineConfig::default()
    })
    .expect("the preparation engine must initialize")
}
pub fn request() -> Initialization {
    Initialization {
        runtime_config: b"{}".to_vec(),
        config: b"{}".to_vec(),
        sandbox: InitSandbox::Sealed,
        cpu_ms: 1000,
        wall_ms: 5000,
        max_passes: 4,
        log_bytes: 1024,
    }
}
pub fn guest(imports: &str, init: &str) -> Vec<u8> {
    guest_with_handle(imports, init, "(i64.const 0)")
}

pub fn guest_with_handle(imports: &str, init: &str, handle: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module {imports}
      (memory (export "memory") 1 64)
      (global $state (mut i32) (i32.const 1))
      (table (export "table") 1 funcref)
      (elem (i32.const 0) $function)
      (func $function)
      (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "wisp_handle") (param i32 i32) (result i64) {handle})
      (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "wisp_initialize") {init}))"#
    ))
    .expect("the test guest must assemble")
}
