//! Deploy-time validation. The import list is the capability manifest (V1),
//! and with threads and atomics outside the engine profile (V2) blocking is
//! not expressible inside the sandbox at all. V2 is `wasmparser`'s validator
//! run with [`ENGINE_FEATURES`], the one declaration of the profile the
//! engine is configured from, so the two cannot disagree.

mod checks;
mod report;
mod scan;

use wasmparser::{Validator, WasmFeatures};

pub use checks::limits;
pub use report::{ValidationReport, Violation, ViolationCode};

use checks::{check_bombs, check_exports, check_globals, check_imports, check_memory};
use scan::scan;

/// The WebAssembly proposals the platform speaks, enforced here as V2 and
/// configured into the engine from this one definition.
///
/// * `threads` / `shared_everything_threads` — V2. No threads means no shared
///   memory, no atomics, and therefore **no counting thread**: the measurement
///   primitive most Spectre-class attacks need is not merely discouraged, it
///   fails to validate.
/// * `relaxed_simd` — V2: use the SIMD subset without implementation-dependent
///   result choices.
/// * `exceptions` / `legacy_exceptions` — V2, beyond the profile.
/// * `stack_switching` — the ABI is callback-CPS, with no engine stack
///   suspension. Engine-suspended stacks are exactly the thing invariant 3
///   exists to forbid, so the proposal that reintroduces them is off at the
///   bytecode level.
/// * `tail_call` — off as a conservative reading; a tail call to an import
///   is still covered by V1.
/// * `multi_memory` / `memory64` — exactly one linear memory, 32-bit. V3 says
///   the same thing structurally; both are checked.
/// * `gc` / `gc_types` — the guest ships its own GC inside linear memory, so
///   engine GC types would be a second heap the platform cannot see.
///   Invariant 1 says all tenant state is visible in linear memory + globals +
///   tables; wasm-GC objects would break that. Turning `gc_types` off also
///   removes `externref`, which is the right answer for a different reason: no
///   host pointers ever enter guest memory, and every host resource in this
///   platform is an opaque `u32` handle instead.
/// * `component_model` — the world is lowered to a flat core-wasm ABI under
///   module `isoplane`.
pub const ENGINE_FEATURES: WasmFeatures = {
    let f = WasmFeatures::WASM2
        .union(WasmFeatures::EXTENDED_CONST)
        .union(WasmFeatures::MULTI_VALUE)
        .union(WasmFeatures::SIGN_EXTENSION)
        .union(WasmFeatures::SATURATING_FLOAT_TO_INT)
        .union(WasmFeatures::BULK_MEMORY)
        .union(WasmFeatures::REFERENCE_TYPES)
        .union(WasmFeatures::SIMD);
    f.difference(
        WasmFeatures::THREADS
            .union(WasmFeatures::SHARED_EVERYTHING_THREADS)
            .union(WasmFeatures::RELAXED_SIMD)
            .union(WasmFeatures::EXCEPTIONS)
            .union(WasmFeatures::LEGACY_EXCEPTIONS)
            .union(WasmFeatures::STACK_SWITCHING)
            .union(WasmFeatures::TAIL_CALL)
            .union(WasmFeatures::MULTI_MEMORY)
            .union(WasmFeatures::MEMORY64)
            .union(WasmFeatures::GC)
            .union(WasmFeatures::GC_TYPES)
            .union(WasmFeatures::CUSTOM_PAGE_SIZES)
            .union(WasmFeatures::MEMORY_CONTROL)
            .union(WasmFeatures::WIDE_ARITHMETIC)
            .union(WasmFeatures::COMPONENT_MODEL),
    )
};

/// Every violation, not the first: a tenant learns everything wrong with an
/// artifact in one round trip.
pub fn validate(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    validate_with_memory_limit(wasm, memory_limit, None)
}

/// Validates an explicitly marked local debugger artifact with all sandbox checks retained.
pub fn validate_local_debug(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
    memory: Option<isoplane_abi::config::GuestMemoryBytes>,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    validate_for_execution(
        wasm,
        memory_limit,
        memory,
        true,
        &isoplane_abi::config::ValidationLimits::default(),
    )
}

/// Validate with a fallback memory allowance and an optional explicit declaration ceiling.
pub fn validate_with_memory_limit(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
    memory: Option<isoplane_abi::config::GuestMemoryBytes>,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    validate_with_limits(
        wasm,
        memory_limit,
        memory,
        &isoplane_abi::config::ValidationLimits::default(),
    )
}

/// Validates with operator-selected structural and memory limits.
pub fn validate_with_limits(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
    memory: Option<isoplane_abi::config::GuestMemoryBytes>,
    limits: &isoplane_abi::config::ValidationLimits,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    validate_for_execution(wasm, memory_limit, memory, false, limits)
}

/// Validates a local debugger artifact with operator-selected limits.
pub fn validate_local_debug_with_limits(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
    memory: Option<isoplane_abi::config::GuestMemoryBytes>,
    limits: &isoplane_abi::config::ValidationLimits,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    validate_for_execution(wasm, memory_limit, memory, true, limits)
}

fn validate_for_execution(
    wasm: &[u8],
    memory_limit: isoplane_abi::config::GuestMemoryBytes,
    memory: Option<isoplane_abi::config::GuestMemoryBytes>,
    local_debug: bool,
    limits: &isoplane_abi::config::ValidationLimits,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    let mut v = Vec::new();
    let mut report = ValidationReport {
        module_bytes: wasm.len() as u64,
        ..Default::default()
    };

    // The header says component or module, and the rejection can say which.
    if is_component(wasm) {
        return Err(vec![Violation::new(
            ViolationCode::V1UndeclaredImport,
            "artifact is a WebAssembly component; this platform lowers isoplane:platform@1 to a \
             flat core-wasm ABI under module `isoplane` and links no component imports",
        )]);
    }

    match scan(wasm) {
        Ok(scan) => {
            if scan.local_debug != local_debug {
                v.push(Violation::about(ViolationCode::LocalDebugArtifact,
                    isoplane_abi::LOCAL_DEBUG_SECTION,
                    if scan.local_debug {
                        "local debugger artifacts cannot be deployed or prepared; publish without local debugging"
                    } else {
                        "local debugging requires an explicitly marked raw debug artifact"
                    }));
            }

            check_imports(&scan, &mut report, &mut v, limits);
            check_exports(&scan, &mut report, &mut v, limits);
            check_globals(&scan, &mut v);
            check_memory(
                &scan,
                memory.map_or(
                    memory_limit.get(),
                    isoplane_abi::config::GuestMemoryBytes::get,
                ),
                memory.is_some(),
                &mut report,
                &mut v,
            );
            check_bombs(&scan, &mut report, &mut v, limits);
        }
        Err(e) => {
            v.push(Violation::new(
                ViolationCode::V2ForbiddenFeature,
                format!("binary is not a well-formed core module: {e}"),
            ));
        }
    }

    // The structural scan runs first so a module that fails V2 still gets a
    // complete report; a second parse with every feature on tells "uses a
    // proposal the profile turns off" from "malformed".
    if let Err(e) = Validator::new_with_features(ENGINE_FEATURES).validate_all(wasm) {
        let malformed = Validator::new_with_features(WasmFeatures::all())
            .validate_all(wasm)
            .is_err();
        let detail = if malformed {
            format!("bytecode validation failed: {}", e.message())
        } else {
            format!(
                "requires a WebAssembly proposal disabled by the Isoplane engine profile (V2): {}",
                e.message()
            )
        };
        v.push(Violation::new(ViolationCode::V2ForbiddenFeature, detail));
    }

    if v.is_empty() {
        Ok(report)
    } else {
        Err(v)
    }
}

/// A component's layer field (bytes 6..8) is `01 00`; a core module's is
/// `00 00`.
fn is_component(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[0..4] == [0x00, 0x61, 0x73, 0x6d] && wasm[6..8] == [0x01, 0x00]
}
