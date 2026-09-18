//! Local ahead-of-time compilation using the validator's feature set.

use std::time::Instant;

use crate::validator;
use isoplane_abi::host::CodeTier as Tier;

pub struct Compiled {
    pub bytes: Vec<u8>,
    pub compile_ms: u64,
    /// The engine and compiler that produced this native output.
    pub engine: String,
}

/// Compile bytes that have already passed validation. The fast tier is
/// Winch, falling back to unoptimized Cranelift where Winch does not cover a
/// proposal on this host; the optimized tier is Cranelift at speed.
pub fn compile(wasm: &[u8], tier: Tier) -> anyhow::Result<Compiled> {
    let mut cfg = wasmtime::Config::new();
    engine_profile(&mut cfg);
    let engine_label = match tier {
        Tier::Fast => {
            cfg.strategy(wasmtime::Strategy::Winch);
            "wasmtime-48/winch"
        }
        Tier::Optimized => {
            cfg.strategy(wasmtime::Strategy::Cranelift);
            cfg.cranelift_opt_level(wasmtime::OptLevel::Speed);
            "wasmtime-48/cranelift-speed"
        }
    };
    let clock = Instant::now();
    match wasmtime::Engine::new(&cfg).and_then(|e| e.precompile_module(wasm)) {
        Ok(bytes) => Ok(Compiled {
            bytes,
            compile_ms: clock.elapsed().as_millis() as u64,
            engine: engine_label.to_string(),
        }),
        Err(e) if tier == Tier::Fast => {
            tracing::warn!(error = %e, "baseline compiler unavailable; falling back to cranelift/none");
            let mut cfg = wasmtime::Config::new();
            engine_profile(&mut cfg);
            cfg.strategy(wasmtime::Strategy::Cranelift);
            cfg.cranelift_opt_level(wasmtime::OptLevel::None);
            let clock = Instant::now();
            let bytes = wasmtime::Engine::new(&cfg)?.precompile_module(wasm)?;
            Ok(Compiled {
                bytes,
                compile_ms: clock.elapsed().as_millis() as u64,
                engine: "wasmtime-48/cranelift-none".to_string(),
            })
        }
        Err(e) => Err(e.into()),
    }
}

/// The engine configured with the feature set the validator enforces, from
/// the one definition of it.
pub fn engine_profile(cfg: &mut wasmtime::Config) {
    let f = validator::ENGINE_FEATURES;
    // The threads and stack-switching proposals are configured through the
    // feature-set API because their dedicated setters exist only when the
    // matching wasmtime cargo features are compiled in, which they are not.
    cfg.wasm_features(wasmtime::WasmFeatures::THREADS, f.threads());
    cfg.wasm_relaxed_simd(f.relaxed_simd());
    cfg.wasm_simd(f.simd());
    cfg.wasm_bulk_memory(f.bulk_memory());
    cfg.wasm_reference_types(f.reference_types());
    cfg.wasm_multi_memory(f.multi_memory());
    cfg.wasm_memory64(f.memory64());
    cfg.wasm_tail_call(f.tail_call());
    cfg.wasm_gc(f.gc());
    cfg.wasm_function_references(f.function_references());
    cfg.wasm_features(wasmtime::WasmFeatures::STACK_SWITCHING, f.stack_switching());
    cfg.wasm_extended_const(f.extended_const());
    cfg.cranelift_opt_level(wasmtime::OptLevel::Speed);
    cfg.wasm_multi_value(f.multi_value());
}
