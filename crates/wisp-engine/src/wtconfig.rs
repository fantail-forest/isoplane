//! Configure Wasmtime sandbox engines and pooling allocations.

use anyhow::{Context, Result};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, OptLevel, PoolingAllocationConfig, WasmFeatures,
};

use crate::EngineConfig;

/// Metadata byte limit for each pooled core instance.
/// Managed guests with more runtime functions need a larger allowance;
/// instantiation fails when a module exceeds this limit.
pub(crate) const MAX_CORE_INSTANCE_SIZE: usize = 24 * 1024;

/// Element limit for each pooled table.
pub(crate) const TABLE_ELEMENTS: usize = 1024;

/// Configured core-instance cap plus one pointer-sized entry per table element.
pub(crate) const BOOKKEEPING_BYTES: usize =
    MAX_CORE_INSTANCE_SIZE + TABLE_ELEMENTS * std::mem::size_of::<usize>();

pub(crate) fn build_engine(cfg: &EngineConfig) -> Result<Engine> {
    let mut c = Config::new();

    // Disable these proposals explicitly even when Cargo features enable their implementations.
    c.wasm_features(
        WasmFeatures::THREADS
            | WasmFeatures::SHARED_EVERYTHING_THREADS
            | WasmFeatures::STACK_SWITCHING,
        false,
    );
    c.wasm_relaxed_simd(false);
    c.relaxed_simd_deterministic(true);
    c.wasm_simd(true);
    c.wasm_multi_memory(false);
    c.wasm_memory64(false);
    c.wasm_tail_call(false);
    c.wasm_bulk_memory(true);
    c.wasm_reference_types(true);
    c.wasm_multi_value(true);
    c.wasm_extended_const(true);
    // Wasm GC references have no snapshot representation.
    c.wasm_gc(false);
    c.wasm_function_references(false);
    c.wasm_custom_page_sizes(false);
    c.wasm_wide_arithmetic(false);

    c.epoch_interruption(true);
    // Fuel would be a second compute meter.
    c.consume_fuel(false);

    c.memory_init_cow(true);
    c.memory_reservation(cfg.max_memory_bytes);
    c.memory_reservation_for_growth(0);
    c.memory_may_move(false);

    // A guard region lets Cranelift omit explicit bounds checks for suitable static offsets.
    c.memory_guard_size(cfg.guard_size_bytes);
    c.guard_before_linear_memory(false);
    c.cranelift_opt_level(OptLevel::Speed);
    if cfg.spectre_mitigations {
        // Explicit options keep these mitigations enabled when requested.
        // SAFETY: both are recognised Cranelift settings with boolean values;
        // an unknown name or bad value would be caught by `Engine::new` below.
        unsafe {
            c.cranelift_flag_set("enable_heap_access_spectre_mitigation", "true");
            c.cranelift_flag_set("enable_table_access_spectre_mitigation", "true");
        }
    }

    // Trap backtraces support diagnostics.
    c.wasm_backtrace_max_frames(std::num::NonZeroUsize::new(32));
    c.native_unwind_info(true);

    if cfg.pooling {
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling(cfg)));
    }

    let engine = Engine::new(&c).map_err(anyhow::Error::from).with_context(|| {
        format!(
            "wasmtime engine configuration rejected (max_instances={}, max_memory={} MiB, \
             guard={} MiB): the pooling allocator reserves \
             max_instances * (max_memory + guard) of virtual address space",
            cfg.max_instances,
            cfg.max_memory_bytes >> 20,
            cfg.guard_size_bytes >> 20,
        )
    })?;

    Ok(engine)
}

fn pooling(cfg: &EngineConfig) -> PoolingAllocationConfig {
    let mut p = PoolingAllocationConfig::new();
    p.total_core_instances(cfg.max_instances);
    p.total_memories(cfg.max_instances);
    p.total_tables(cfg.max_instances);
    p.max_memory_size(cfg.max_memory_bytes as usize);
    p.max_memories_per_module(1);
    p.max_tables_per_module(4);
    p.table_elements(TABLE_ELEMENTS);
    p.max_core_instance_size(MAX_CORE_INSTANCE_SIZE);
    p
}

/// Artifact-cache key derived from selected engine settings.
pub(crate) fn engine_fingerprint(cfg: &EngineConfig) -> String {
    let desc = format!(
        "wasmtime=48;target={};spectre={};guard={};maxmem={};pooling={}",
        std::env::consts::ARCH,
        cfg.spectre_mitigations,
        cfg.guard_size_bytes,
        cfg.max_memory_bytes,
        cfg.pooling,
    );
    blake3::hash(desc.as_bytes()).to_hex().to_string()[..16].to_string()
}
