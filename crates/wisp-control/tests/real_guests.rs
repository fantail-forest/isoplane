//! The control plane against the **guest team's real modules** (`guests/out/`).
//!
//! Every other test in this crate uses hand-written `.wat`, which proves the
//! rules but not that the rules and a real toolchain agree. These do: they run
//! the actual artifacts the `guest` crate builds through V1-V4 and, for the
//! §4.4 fixture, through the whole deferral loop.
//!
//! The guests are another team's deliverable and may not be built yet, so each
//! test skips loudly rather than failing when its artifact is missing. A
//! skipped test is visible in the output; a deleted one is not.

use std::path::PathBuf;

use wisp_abi::config::Limits;
use wisp_control::preinit::{PreInitRequest, PreInitializer};
use wisp_control::validator::{self, ViolationCode as V};

fn guest(name: &str) -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../guests/out")
        .join(format!("{name}.wasm"));
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!("skipped: {} not built", p.display());
            None
        }
    }
}

fn codes(wasm: &[u8]) -> Vec<V> {
    match validator::validate(wasm, Limits::default()) {
        Ok(r) => {
            println!("    VALID: {}", r.manifest().replace('\n', "\n    "));
            vec![]
        }
        Err(vs) => {
            for v in &vs {
                println!("    -> {v}");
            }
            vs.iter().map(|v| v.code).collect()
        }
    }
}

/// **C6**, against a module a compiler actually produced: an undeclared import
/// is a deploy rejection.
#[test]
fn the_undeclared_import_guest_is_rejected() {
    for name in ["undeclared-import", "foreign-module"] {
        let Some(wasm) = guest(name) else { continue };
        println!("{name}.wasm ({} bytes):", wasm.len());
        let got = codes(&wasm);
        assert!(
            got.contains(&V::V1UndeclaredImport),
            "{name} must be rejected by V1, got {got:?}"
        );
    }
}

/// The well-behaved guests must *pass* — a validator that rejects everything
/// would satisfy every negative test in this crate and be worthless.
#[test]
fn the_well_behaved_guests_validate() {
    let mut seen = 0;
    for name in ["quotes-api", "bigheap", "misbehaver", "impure-cctor"] {
        let Some(wasm) = guest(name) else { continue };
        seen += 1;
        println!("{name}.wasm ({} bytes):", wasm.len());
        let report = validator::validate(&wasm, Limits::default())
            .unwrap_or_else(|vs| panic!("{name} was rejected: {vs:?}"));
        println!(
            "    {} imports, memory {:?}..{:?} pages, {} functions, {} custom-section bytes",
            report.capabilities.len(),
            report.memory_pages_initial,
            report.memory_pages_max,
            report.function_count,
            report.custom_section_bytes
        );
        println!("    {}", report.manifest().replace('\n', "\n    "));
        // `misbehaver` is the C7 spinner: busy-waiting is *expressible*, and
        // deliberately so — §8.3 says the residue is handled at runtime by the
        // CPU budget, not at deploy time by a static analysis nobody can write.
        assert!(!report.capabilities.is_empty() || name == "bigheap");
    }
    if seen == 0 {
        eprintln!("skipped: no guests built");
    }
}

/// §4.4 end to end against the guest SDK's own deferral machinery: the pure
/// initializer pre-runs into the template, the impure one is deferred by
/// replay, and the host learns which capability caused it.
#[test]
fn the_impure_cctor_guest_defers_and_converges() {
    let Some(wasm) = guest("impure-cctor") else {
        return;
    };

    // It must be deployable first: pre-init only ever sees validated bytes.
    let report = validator::validate(&wasm, Limits::default()).expect("impure-cctor must validate");
    assert!(
        report.has_initialize(),
        "the guest SDK must export wisp_initialize"
    );

    let req = PreInitRequest {
        module_hash: wisp_engine::ModuleHash::of(&wasm),
        init_blob: Vec::new(),
    };
    let out = PreInitializer::new()
        .run(&wasm, &req)
        .expect("pre-init must converge");

    println!(
        "impure-cctor.wasm: {} bytes -> template {} bytes, {} pass(es), deferred {:?} because {:?}, \
         {} refusals, seed read {}x, {} ms",
        wasm.len(),
        out.template_bytes,
        out.passes,
        out.deferred_units,
        out.deferred_reasons,
        out.capability_refusals,
        out.seed_reads,
        out.init_cpu_ms
    );
    for l in &out.init_logs {
        println!("    init log: {l}");
    }

    // The template is a *different module*: data segments replaced by the
    // post-init image, and `wisp_initialize` stripped (it has already run, and
    // a second way to re-run static init inside a live instance would let
    // memory drift from the handle manifest).
    assert_ne!(
        out.template_wasm, wasm,
        "the template must not be the input module"
    );
    assert!(
        !exports_init(&out.template_wasm),
        "`wisp_initialize` must be stripped from the template"
    );
    assert!(
        out.passes >= 2,
        "an impure initializer must cost at least one replay"
    );
    assert_eq!(
        out.deferred_units.len(),
        1,
        "exactly one unit is impure in this fixture"
    );
    // The guest's impure unit reaches for the clock; the host must name it.
    assert_eq!(
        out.deferred_reasons[0].1, "clock_now_coarse",
        "the deferral reason must name the capability that caused it"
    );
    assert_eq!(out.capability_refusals, 1);
}

fn exports_init(wasm: &[u8]) -> bool {
    wasmparser::Parser::new(0).parse_all(wasm).any(|p| match p {
        Ok(wasmparser::Payload::ExportSection(r)) => r
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.name == wisp_abi::imports::exports::INITIALIZE),
        _ => false,
    })
}
