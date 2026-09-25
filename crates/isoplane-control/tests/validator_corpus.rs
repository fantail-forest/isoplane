//! Admission fixtures for host capabilities, execution features and structural limits.
//! A rejected fixture can report more than one violation code.

use isoplane_abi::config::Limits;
use isoplane_control::validator::{self, ViolationCode as V};

#[path = "common/validate.rs"]
mod validate;
use validate::{assert_violates, codes, parse, REQUIRED_EXPORTS};

// ---------------------------------------------------------------------------
// fixture construction
// ---------------------------------------------------------------------------

/// A well-formed module that asks for a realistic slice of the capability
/// table with correct signatures.
fn clean_wat() -> String {
    format!(
        r#"(module
  (import "isoplane" "log_emit"         (func $log   (param i32 i32 i32)))
  (import "isoplane" "clock_now_coarse" (func $now   (result i64)))
  (import "isoplane" "entropy_fill"     (func $rand  (param i32 i32)))
  (import "isoplane" "http_fetch"       (func $fetch (param i32 i32) (result i64)))
  {REQUIRED_EXPORTS}
  (func (export "isoplane_gc_hint") (param i32))
  (func $use (drop (call $now)))
)"#
    )
}

// ---------------------------------------------------------------------------
// the corpus
// ---------------------------------------------------------------------------

#[test]
fn clean_module_validates_and_reports_its_capabilities() {
    let wasm = parse(&clean_wat());
    let report = validator::validate(&wasm, Limits::default()).expect("clean module must validate");

    // Capabilities are reported in IMPORTS-table order, not module order: the
    // manifest is the platform's document (invariant 4), not the tenant's.
    assert_eq!(
        report.capabilities,
        vec![
            "log_emit",
            "clock_now_coarse",
            "entropy_fill",
            "http_fetch",
        ]
    );
    assert_eq!(report.memory_pages_initial, Some(17));
    assert_eq!(report.memory_pages_max, Some(2048));
    assert_eq!(report.optional_exports, vec!["isoplane_gc_hint"]);
    assert!(!report.has_initialize());

    let manifest = report.manifest();
    println!("{manifest}");
    assert!(manifest.contains("egress:http"));
    assert!(manifest.contains("NOT requested"));
}

/// Unlisted imports are rejected during admission.
#[test]
fn v1_undeclared_import() {
    assert_violates(
        "undeclared-isoplane-name",
        &clean_wat().replace(
            r#"(import "isoplane" "http_fetch"       (func $fetch (param i32 i32) (result i64)))"#,
            r#"(import "isoplane" "unlisted_fetch"       (func $fetch (param i32 i32) (result i64)))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    assert_violates(
        "wasi-fd-write",
        &clean_wat().replace(
            r#"(import "isoplane" "log_emit"         (func $log   (param i32 i32 i32)))"#,
            r#"(import "wasi_snapshot_preview1" "fd_write" (func $log (param i32 i32 i32)))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    // There is no dynamic-linking escape hatch: an env/GOT-style import is
    // just another undeclared capability.
    assert_violates(
        "emscripten-dynamic-linking",
        &clean_wat()
            .replace("(func $use", r#"(func $unused) (func $use"#)
            .replace(
                r#"(import "isoplane" "log_emit"         (func $log   (param i32 i32 i32)))"#,
                r#"(import "env" "__indirect_function_table" (table 1 funcref))
  (import "isoplane" "log_emit"         (func $log   (param i32 i32 i32)))"#,
            ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
}

/// Declared imports must match the canonical host signature.
#[test]
fn v1_right_name_wrong_signature() {
    assert_violates(
        "http_fetch-wrong-arity",
        &clean_wat().replace(
            r#"(import "isoplane" "http_fetch"       (func $fetch (param i32 i32) (result i64)))"#,
            r#"(import "isoplane" "http_fetch"       (func $fetch (param i32) (result i64)))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
    assert_violates(
        "http_fetch-wrong-result-type",
        &clean_wat()
            .replace(
                r#"(import "isoplane" "http_fetch"       (func $fetch (param i32 i32) (result i64)))"#,
                r#"(import "isoplane" "http_fetch"       (func $fetch (param i32 i32) (result i32)))"#,
            ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
    // Imported memories and globals are not admitted guest capabilities.
    assert_violates(
        "import-a-global-named-like-an-import",
        &clean_wat()
            .replace(
                r#"(import "isoplane" "clock_now_coarse" (func $now   (result i64)))"#,
                r#"(import "isoplane" "clock_now_coarse" (global $now i64))"#,
            )
            .replace("(drop (call $now))", ""),
        Limits::default(),
        V::V1SignatureMismatch,
    );
}

/// **V1.** Missing required exports: the host has no way in.
#[test]
fn v1_missing_exports() {
    let wat = r#"(module
  (memory (export "memory") 1 16)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
)"#;
    println!("fixture `missing-handle-and-wake`:");
    let got = codes(&parse(wat), Limits::default());
    assert_eq!(
        got.iter().filter(|c| **c == V::V1MissingExport).count(),
        2,
        "both isoplane_handle and isoplane_wake should be reported, got {got:?}"
    );

    // An export with the right name and the wrong *kind* is still missing.
    assert_violates(
        "memory-exported-as-a-function",
        &clean_wat()
            .replace(r#"(memory (export "memory") 17 2048)"#, "(memory 17 2048)")
            .replace(
                r#"(func (export "isoplane_alloc")"#,
                r#"(func (export "memory") (param i32) (result i32) (i32.const 0))
  (func (export "isoplane_alloc")"#,
            ),
        Limits::default(),
        V::V1MissingExport,
    );
}

/// Required exports must have the declared host-interface types.
#[test]
fn v1_export_with_the_wrong_signature() {
    assert_violates(
        "isoplane_handle-returns-i32",
        &clean_wat().replace(
            r#"(func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))"#,
            r#"(func (export "isoplane_handle") (param i32 i32) (result i32) (i32.const 1))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
    assert_violates(
        "isoplane_wake-missing-the-result-buffer",
        &clean_wat().replace(
            r#"(func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1))"#,
            r#"(func (export "isoplane_wake") (param i64) (result i64) (i64.const 1))"#,
        ),
        Limits::default(),
        V::V1SignatureMismatch,
    );
}

/// Atomic operations require a proposal excluded by the execution profile.
#[test]
fn v2_atomics_and_threads() {
    assert_violates(
        "atomic-rmw",
        &clean_wat().replace(
            "(func $use",
            r#"(func $spin (result i32)
       (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
    assert_violates(
        "memory.atomic.wait32 — the blocking primitive, inexpressible",
        &clean_wat().replace(
            "(func $use",
            r#"(func $block (result i32)
       (memory.atomic.wait32 (i32.const 0) (i32.const 0) (i64.const -1)))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
    assert_violates(
        "atomic.notify",
        &clean_wat().replace(
            "(func $use",
            r#"(func $wake (result i32)
       (memory.atomic.notify (i32.const 0) (i32.const 1)))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
}

/// The execution profile excludes relaxed SIMD and exceptions.
#[test]
fn v2_relaxed_simd_and_exceptions() {
    assert_violates(
        "relaxed-madd",
        &clean_wat().replace(
            "(func $use",
            r#"(func $rs (result v128)
       (f32x4.relaxed_madd (v128.const i32x4 0 0 0 0)
                           (v128.const i32x4 0 0 0 0)
                           (v128.const i32x4 0 0 0 0)))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
    assert_violates(
        "exception-handling",
        &clean_wat().replace(
            "(func $use",
            r#"(tag $e)
  (func $throws (block $handler (try_table (catch_all $handler) (throw $e))))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
    // Deterministic SIMD is *permitted* — V2 is a scalpel, not a ban on SIMD.
    let ok = clean_wat().replace(
        "(func $use",
        r#"(func $simd (result v128)
       (i32x4.add (v128.const i32x4 1 2 3 4) (v128.const i32x4 1 2 3 4)))
  (func $use"#,
    );
    assert!(
        validator::validate(&parse(&ok), Limits::default()).is_ok(),
        "deterministic SIMD must still validate"
    );
}

/// Tail calls and stack switching are outside this execution profile.
#[test]
fn v2_tail_calls_and_stack_switching_are_off() {
    assert_violates(
        "return_call-to-an-import",
        &clean_wat().replace(
            "(func $use",
            r#"(func $tc (result i64) (return_call $now))
  (func $use"#,
        ),
        Limits::default(),
        V::V2ForbiddenFeature,
    );
}

/// One unshared linear memory must fit the configured memory ceiling.
#[test]
fn v3_memory_shape() {
    assert_violates(
        "two-memories",
        &clean_wat().replace(
            r#"(memory (export "memory") 17 2048)"#,
            r#"(memory (export "memory") 17 2048) (memory 1 1)"#,
        ),
        Limits::default(),
        V::V3MemoryShape,
    );
    assert_violates(
        "shared-memory",
        &clean_wat().replace(
            r#"(memory (export "memory") 17 2048)"#,
            r#"(memory (export "memory") 17 2048 shared)"#,
        ),
        Limits::default(),
        V::V3MemoryShape,
    );
    assert_violates(
        "imported-memory",
        &clean_wat().replace(
            r#"(memory (export "memory") 17 2048)"#,
            r#"(import "isoplane" "memory" (memory 17 2048))"#,
        ),
        Limits::default(),
        V::V1UndeclaredImport,
    );
    // 2049 pages = 128 MiB + 64 KiB: one page over the 128 MiB ceiling.
    assert_violates(
        "max-over-ceiling",
        &clean_wat().replace(
            r#"(memory (export "memory") 17 2048)"#,
            r#"(memory (export "memory") 17 2049)"#,
        ),
        Limits::default(),
        V::V3MemoryShape,
    );
    // The same module is admitted only when its declared maximum fits the configured ceiling.
    let at_standard_ceiling = clean_wat();
    assert!(validator::validate(&parse(&at_standard_ceiling), Limits::default()).is_ok());
    assert_violates(
        "standard-sized module on the free plan",
        &at_standard_ceiling,
        Limits {
            module_bytes_compressed: 5242880,
            package_bytes: 5242880,
            instance_memory_max: 67108864,
            cpu_ms_per_http_dispatch: 10,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6,
        },
        V::V3MemoryShape,
    );
    // A minimum above the ceiling could never be instantiated at all.
    assert_violates(
        "min-over-ceiling",
        &clean_wat().replace(
            r#"(memory (export "memory") 17 2048)"#,
            r#"(memory (export "memory") 4096 4096)"#,
        ),
        Limits::default(),
        V::V3MemoryShape,
    );
}

/// Structural limits reject excessive compilation inputs.
#[test]
fn v4_function_count_bomb() {
    let n = validator::limits::MAX_FUNCTIONS + 1;
    let mut wat = String::with_capacity(n as usize * 8 + 512);
    wat.push_str("(module\n");
    wat.push_str(REQUIRED_EXPORTS);
    for _ in 0..n {
        wat.push_str("(func)");
    }
    wat.push(')');
    println!(
        "fixture `function-count-bomb` ({n} functions, {} bytes of wat):",
        wat.len()
    );
    let got = codes(&parse(&wat), Limits::default());
    assert!(got.contains(&V::V4ResourceBomb), "got {got:?}");
}

#[test]
fn v4_locals_bomb() {
    let n = validator::limits::MAX_LOCALS_PER_FUNCTION + 1;
    let wat = clean_wat().replace(
        "(func $use",
        &format!(
            "(func $fat (local i32) {}) (func $use",
            "(local i32) ".repeat(n as usize - 1)
        ),
    );
    println!("fixture `locals-bomb` ({n} locals):");
    let got = codes(&parse(&wat), Limits::default());
    assert!(got.contains(&V::V4ResourceBomb), "got {got:?}");
}

#[test]
fn v4_table_bomb() {
    assert_violates(
        "table-bomb",
        &clean_wat().replace(
            REQUIRED_EXPORTS,
            &format!("{REQUIRED_EXPORTS}\n  (table 2000000 funcref)"),
        ),
        Limits::default(),
        V::V4ResourceBomb,
    );
}

/// Custom sections also consume admission and compilation resources.
/// Building a trailing section directly keeps the oversized fixture cheap.
#[test]
fn v4_huge_custom_section() {
    let mut wasm = parse(&clean_wat());
    let payload_len = validator::limits::MAX_CUSTOM_SECTION_BYTES as usize + 1;
    let name = b".debug_info_bomb";
    let mut body = Vec::with_capacity(payload_len + 32);
    leb128(&mut body, name.len() as u64);
    body.extend_from_slice(name);
    body.resize(body.len() + payload_len, 0x41);

    wasm.push(0); // section id 0 = custom
    leb128(&mut wasm, body.len() as u64);
    wasm.extend_from_slice(&body);

    println!(
        "fixture `huge-custom-section` ({} MiB module):",
        wasm.len() >> 20
    );
    let got = codes(&wasm, Limits::default());
    assert!(got.contains(&V::V4ResourceBomb), "got {got:?}");
}

fn leb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return;
        }
    }
}

/// Component binaries are not admitted as core WebAssembly modules.
#[test]
fn v1_component_upload_is_rejected() {
    let component = wat::parse_str(r#"(component)"#).unwrap();
    println!("fixture `component`:");
    let got = codes(&component, Limits::default());
    assert_eq!(got, vec![V::V1UndeclaredImport], "got {got:?}");
}

/// Malformed binary input is reported separately from unsupported proposals.
#[test]
fn malformed_binary_is_rejected_as_malformed() {
    let mut wasm = parse(&clean_wat());
    wasm.truncate(wasm.len() - 5);
    match validator::validate(&wasm, Limits::default()) {
        Ok(_) => panic!("corrupt module validated"),
        Err(vs) => {
            let joined = vs
                .iter()
                .map(|v| v.detail.clone())
                .collect::<Vec<_>>()
                .join(" | ");
            println!("    -> {joined}");
            assert!(vs.iter().any(|v| v.code == V::V2ForbiddenFeature));
            assert!(
                !joined.contains("disabled by the Isoplane engine profile"),
                "a corrupt module must not be blamed on a disabled proposal"
            );
        }
    }
}

/// Capability validation uses the configured execution profile.
#[test]
fn engine_profile_has_the_dangerous_proposals_off() {
    let f = validator::ENGINE_FEATURES;
    assert!(
        !f.threads(),
        "threads on would give the guest a counting thread"
    );
    assert!(!f.shared_everything_threads());
    assert!(
        !f.relaxed_simd(),
        "relaxed SIMD is outside the supported execution profile"
    );
    assert!(!f.exceptions());
    assert!(
        !f.stack_switching(),
        "engine-suspended stacks are what invariant 3 forbids"
    );
    assert!(!f.multi_memory());
    assert!(!f.memory64());
    assert!(!f.component_model());
    assert!(!f.gc());
    // ...and the ones a real .NET module needs are on.
    assert!(f.simd());
    assert!(f.bulk_memory());
    assert!(f.reference_types());
    assert!(f.multi_value());
    assert!(f.sign_extension());
}

fn debug_wasm(wat: &str) -> Vec<u8> {
    let marked = wat.replacen(
        "(module",
        &format!(
            "(module (@custom \"{}\")",
            isoplane_abi::LOCAL_DEBUG_SECTION
        ),
        1,
    );
    parse(&marked)
}

#[test]
fn a_local_debug_artifact_is_rejected_by_normal_admission() {
    let wasm = debug_wasm(&clean_wat());
    assert!(
        codes(&wasm, Limits::default()).contains(&V::LocalDebugArtifact),
        "normal deployment must refuse a local debugger artifact"
    );
    assert!(
        validator::validate_local_debug(&wasm, Limits::default(), None).is_ok(),
        "explicit local debug admission must accept an otherwise valid marked module"
    );
}

#[test]
fn explicit_debug_validation_still_rejects_forbidden_imports() {
    let wasm = debug_wasm(&format!(
        "(module (import \"evil\" \"escape\" (func)) {REQUIRED_EXPORTS})"
    ));
    let errors = validator::validate_local_debug(&wasm, Limits::default(), None)
        .expect_err("debug mode cannot bypass sandbox admission");
    assert!(
        errors
            .iter()
            .any(|error| error.code == V::V1UndeclaredImport),
        "the undeclared capability must remain forbidden in local debugging"
    );
}

#[test]
fn explicit_debug_validation_requires_a_marked_artifact() {
    let errors = validator::validate_local_debug(&parse(&clean_wat()), Limits::default(), None)
        .expect_err("normal publication cannot accidentally select debug execution");
    assert!(
        errors
            .iter()
            .any(|error| error.code == V::LocalDebugArtifact),
        "an unmarked artifact must fail the explicit debug gate"
    );
}

#[test]
fn a_memory_override_accepts_a_larger_bounded_module_without_relaxing_capabilities() {
    let budget =
        isoplane_abi::config::GuestMemoryBytes::new(512 << 20).expect("valid memory budget");
    let source = clean_wat().replace("17 2048", "17 8192");
    assert!(
        validator::validate(&parse(&source), Limits::default()).is_err(),
        "the standard default must continue to refuse a 512 MiB maximum"
    );
    assert!(
        validator::validate_with_memory_limit(&parse(&source), Limits::default(), Some(budget))
            .is_ok(),
        "an explicit memory-only budget must admit the bounded larger module"
    );
    let hostile = source.replace("\"clock_now_coarse\"", "\"undeclared_clock\"");
    let failures =
        validator::validate_with_memory_limit(&parse(&hostile), Limits::default(), Some(budget))
            .expect_err("memory authorization cannot grant host capabilities");
    assert!(
        failures
            .iter()
            .any(|failure| failure.code == V::V1UndeclaredImport),
        "an independent capability violation must remain rejected"
    );
}

#[test]
fn an_explicit_memory_budget_refuses_unbounded_or_larger_memory() {
    let budget = isoplane_abi::config::GuestMemoryBytes::new(128 << 20).expect("valid budget");
    for memory in ["17", "17 2049", "2049 2049"] {
        let source = clean_wat().replace("17 2048", memory);
        let failures =
            validator::validate_with_memory_limit(&parse(&source), Limits::default(), Some(budget))
                .expect_err("memory shape must enforce the explicit ceiling");
        assert!(
            failures
                .iter()
                .any(|failure| failure.code == V::V3MemoryShape),
            "memory shape {memory} must fail the authorized ceiling"
        );
    }
}

#[test]
fn local_debug_validation_applies_only_the_authorized_memory_override() {
    let budget = isoplane_abi::config::GuestMemoryBytes::new(512 << 20).unwrap();
    let source = clean_wat().replace("17 2048", "17 8192");
    let wasm = debug_wasm(&source);
    assert!(
        validator::validate_local_debug(&wasm, Limits::default(), None).is_err(),
        "local debugging must retain the default plan ceiling without an override"
    );
    assert!(
        validator::validate_local_debug(&wasm, Limits::default(), Some(budget)).is_ok(),
        "the explicit local debug ceiling must admit a larger bounded module"
    );
    let hostile = debug_wasm(&source.replace("clock_now_coarse", "undeclared_clock"));
    let errors =
        validator::validate_local_debug(&hostile, Limits::default(), Some(budget)).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.code == V::V1UndeclaredImport),
        "debug memory authorization must not grant an undeclared capability"
    );
}

#[test]
fn optional_initialization_requires_the_canonical_poll_result_signature() {
    for definition in [
        r#"(func (export "isoplane_initialize"))"#,
        r#"(func (export "isoplane_initialize") (param i32) (result i64) i64.const 0)"#,
        r#"(func (export "isoplane_initialize") (result i32) i32.const 0)"#,
    ] {
        let wasm = parse(&format!("(module {REQUIRED_EXPORTS} {definition})"));
        let violations = validator::validate(&wasm, Limits::default())
            .expect_err("malformed optional initialization signatures must fail admission");
        assert_eq!(
            violations.len(),
            1,
            "the fixture must fail only its initialization signature"
        );
        assert_eq!(
            violations[0].code,
            V::V1SignatureMismatch,
            "initialization signature rejection must happen before engine activation"
        );
    }
    let wasm = parse(&format!(
        r#"(module {REQUIRED_EXPORTS}
        (func (export "isoplane_initialize") (result i64) i64.const 0))"#
    ));
    assert!(
        validator::validate(&wasm, Limits::default())
            .expect("canonical initializer")
            .has_initialize(),
        "a correctly typed asynchronous initializer must be reported as available"
    );
}

#[test]
fn optional_exports_cannot_disguise_nonfunction_objects_as_guest_entrypoints() {
    for name in isoplane_abi::exports::OPTIONAL {
        let wasm = parse(&format!(
            r#"(module {REQUIRED_EXPORTS}
            (global (export "{name}") i32 (i32.const 0)))"#
        ));
        let violations = validator::validate(&wasm, Limits::default())
            .expect_err("an optional entrypoint must be a function when present");
        assert_eq!(
            violations.len(),
            1,
            "the fixture must fail only its optional export kind"
        );
        assert_eq!(
            violations[0].code,
            V::V1MissingExport,
            "a nonfunction object cannot fulfill an optional entrypoint contract"
        );
    }
}
