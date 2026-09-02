//! Configure Wasmtime sandbox engines and pooling allocations.

use std::hash::{Hash, Hasher};

use anyhow::{bail, Context, Result};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, OptLevel, PoolingAllocationConfig, WasmFeatures,
};

use crate::EngineConfig;

/// Minimum threshold used when requesting copy-on-write images for sparse data.
pub(crate) const DENSE_IMAGE_FLOOR: u64 = 128 << 20;

/// Copy-on-write image threshold covering the configured memory allowance.
pub(crate) fn dense_image_bytes(cfg: &EngineConfig) -> u64 {
    DENSE_IMAGE_FLOOR.max(cfg.max_memory_bytes)
}

/// Metadata byte limit for each pooled core instance.
/// Managed guests with more runtime functions need a larger allowance;
/// instantiation fails when a module exceeds this limit.
pub(crate) const MAX_CORE_INSTANCE_SIZE: usize = 4 * 1024 * 1024;

/// Element limit for each pooled table.
pub(crate) const TABLE_ELEMENTS: usize = 131072;

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
    c.memory_guaranteed_dense_image_size(dense_image_bytes(cfg));
    c.memory_reservation(cfg.max_memory_bytes);
    c.memory_reservation_for_growth(0);
    c.memory_may_move(false);

    c.memory_guard_size(cfg.guard_size_bytes);
    c.guard_before_linear_memory(false);
    c.cranelift_opt_level(OptLevel::Speed);
    if cfg.spectre_mitigations {
        // Both default to on in Cranelift; set explicitly so a default flip
        // cannot silently disarm a security control.
        // SAFETY: both are recognised Cranelift settings with boolean values;
        // an unknown name or bad value is caught by `Engine::new` below.
        unsafe {
            c.cranelift_flag_set("enable_heap_access_spectre_mitigation", "true");
            c.cranelift_flag_set("enable_table_access_spectre_mitigation", "true");
        }
    }

    // Trap messages feed `TrapKind` classification and tenant-facing
    // telemetry.
    c.wasm_backtrace_max_frames(std::num::NonZeroUsize::new(32));
    c.native_unwind_info(true);

    if cfg.pooling {
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(pooling(cfg)));
    }

    Engine::new(&c)
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!(
                "wasmtime engine configuration rejected (max_instances={}, max_memory={} MiB, \
                 guard={} MiB): the pooling allocator reserves \
                 max_instances * (max_memory + guard) of virtual address space",
                cfg.max_instances,
                cfg.max_memory_bytes >> 20,
                cfg.guard_size_bytes >> 20,
            )
        })
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

/// `(module (memory 1 1 shared))`, as bytes so the probe needs no assembler at
/// runtime. The tests below pin the encoding against the text form.
const SHARED_MEMORY_PROBE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x05, 0x04, 0x01, 0x03, 0x01, 0x01, // memory section: 1 memory, shared, min 1, max 1
];
/// `(module (memory 1) (func (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))))`.
const ATOMICS_PROBE: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
    0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type section: () -> ()
    0x03, 0x02, 0x01, 0x00, // function section: one function of type 0
    0x05, 0x03, 0x01, 0x00, 0x01, // memory section: 1 memory, min 1
    0x0a, 0x0d, 0x01, 0x0b, 0x00, // code section: one body of 11 bytes, no locals
    0x41, 0x00, 0x41, 0x01, // i32.const 0, i32.const 1
    0xfe, 0x1e, 0x02, 0x00, // i32.atomic.rmw.add align=2 offset=0
    0x1a, 0x0b, // drop, end
];

/// Rejects engines that validate the shared-memory or atomic-operation probes.
pub(crate) fn assert_feature_set(engine: &Engine) -> Result<()> {
    for (what, wasm) in [
        ("shared memory", SHARED_MEMORY_PROBE),
        ("atomics", ATOMICS_PROBE),
    ] {
        if wasmtime::Module::validate(engine, wasm).is_ok() {
            bail!(
                "the engine accepts {what}, which the sandbox forbids; a threads proposal has \
                 been re-enabled in the wasmtime configuration"
            );
        }
    }
    Ok(())
}

/// Artifact-cache key derived from Wasmtime compatibility inputs and selected engine settings.
pub(crate) fn engine_fingerprint(engine: &Engine, cfg: &EngineConfig) -> String {
    let mut h = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut h);
    cfg.spectre_mitigations.hash(&mut h);
    cfg.guard_size_bytes.hash(&mut h);
    cfg.max_memory_bytes.hash(&mut h);
    cfg.pooling.hash(&mut h);
    dense_image_bytes(cfg).hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_threshold_covers_the_configured_memory_ceiling() {
        for ceiling in [67108864, 134217728, 536870912] {
            let cfg = EngineConfig {
                max_memory_bytes: ceiling,
                ..EngineConfig::default()
            };
            assert!(
                dense_image_bytes(&cfg) >= ceiling,
                "image threshold {} B is below the configured {ceiling}-byte memory ceiling",
                dense_image_bytes(&cfg),
            );
        }
    }

    #[test]
    fn the_image_threshold_keeps_its_floor_for_small_memory_limits() {
        let cfg = EngineConfig {
            max_memory_bytes: 32 << 20,
            ..EngineConfig::default()
        };
        assert_eq!(
            dense_image_bytes(&cfg),
            DENSE_IMAGE_FLOOR,
            "the image threshold must retain the configured floor for small memory limits"
        );
    }

    #[test]
    fn the_probe_bytes_are_the_modules_their_comments_name() {
        assert_eq!(
            wat::parse_str("(module (memory 1 1 shared))").expect("wat"),
            SHARED_MEMORY_PROBE,
            "the shared-memory probe bytes must encode the module the comment names"
        );
        assert_eq!(
            wat::parse_str(
                "(module (memory 1) (func (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))))"
            )
            .expect("wat"),
            ATOMICS_PROBE,
            "the atomics probe bytes must encode the module the comment names"
        );
    }

    #[test]
    fn the_feature_probe_fires_on_an_engine_that_accepts_threads() {
        let mut c = Config::new();
        c.wasm_features(WasmFeatures::THREADS, true);
        let permissive = Engine::new(&c).expect("a default engine builds");
        assert!(
            assert_feature_set(&permissive).is_err(),
            "an engine with the threads proposal on must be refused"
        );
    }

    #[test]
    fn the_feature_probe_passes_the_engine_this_crate_builds() {
        let engine = build_engine(&EngineConfig {
            max_instances: 4,
            max_memory_bytes: 4 << 20,
            guard_size_bytes: 2 << 20,
            ..EngineConfig::default()
        })
        .expect("the engine builds");
        assert_feature_set(&engine).expect("the configured engine rejects threads and atomics");
    }
}
