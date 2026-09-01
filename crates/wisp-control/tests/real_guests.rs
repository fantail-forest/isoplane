//! The control plane against the **guest team's real modules** (`guests/out/`).
//!
//! Every other test in this crate uses hand-written `.wat`, which proves the
//! rules but not that the rules and a real toolchain agree. These do: they run
//! the actual artifacts the `guest` crate builds through V1-V4 and, for the
//! pre-init fixture, through the whole deferral loop.
//!
//! The guests are another team's deliverable and may not be built yet, so each
//! test skips loudly rather than failing when its artifact is missing. A
//! skipped test is visible in the output; a deleted one is not.

use std::path::PathBuf;

use wisp_abi::config::Limits;
use wisp_control::preinit::{PreInitRequest, PreInitializer};
use wisp_control::validator::{self, ViolationCode as V};

/// Where `guests/build.sh` writes its modules.
fn guest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../guests/out")
}

fn guest(name: &str) -> Option<Vec<u8>> {
    let p = guest_dir().join(format!("{name}.wasm"));
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!("skipped: {} not built", p.display());
            None
        }
    }
}

/// A module `guests/out` may hold, and what the validator must say about it.
struct Guest {
    /// File stem. The file is `guests/out/<name>.wasm`.
    name: &'static str,
    /// Every violation `validate(.., default numeric limits)` must report, as
    /// `(code, subject)`. Empty means the module must be ACCEPTED.
    standard: &'static [(V, &'static str)],
}

const fn ok(name: &'static str) -> Guest {
    Guest {
        name,
        standard: &[],
    }
}

/// **The complete contents of `guests/out`, and the verdict each module must
/// draw.** A file on disk that is not named here fails this test, and so does
/// an expected fixture that is not on disk — an unlisted module is a
/// capability surface nobody has stated an opinion about.
const GUESTS: &[Guest] = &[
    // The application guests.
    ok("quotes-api"),
    ok("misbehaver"),
    ok("bigheap"),
    ok("impure-cctor"),
    Guest {
        name: "undeclared-import",
        // Right module, wrong field.
        standard: &[(V::V1UndeclaredImport, "wisp.fs_open")],
    },
    Guest {
        name: "foreign-module",
        // Wrong module entirely.
        standard: &[(V::V1UndeclaredImport, "env.__wasi_fd_write")],
    },
];

/// Validate each real guest against its expected capability and memory-limit verdict.
#[test]
fn every_guest_in_guests_out_draws_the_verdict_it_is_supposed_to() {
    let dir = guest_dir();
    let mut on_disk: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter_map(|n| n.strip_suffix(".wasm").map(str::to_string))
            .collect(),
        Err(e) => {
            eprintln!(
                "SKIP: cannot read {} ({e}), so no guest was validated. `guests/out/*.wasm` is \
                 gitignored and needs the wasm toolchain; build it with \
                 `scripts/dev guests/build.sh`.",
                dir.display()
            );
            return;
        }
    };
    on_disk.sort();

    if on_disk.is_empty() {
        eprintln!(
            "SKIP: {} holds no modules, so nothing was validated — not that the real guests \
             deploy, and not that `undeclared-import.wasm` and `foreign-module.wasm` still \
             fail. Build them with `scripts/dev guests/build.sh`.",
            dir.display()
        );
        return;
    }

    // Every discrepancy is collected and reported together. One `assert` per
    // module would report the first module that moved and hide the rest,
    // which is precisely the shape of failure this test exists to make legible
    // — a capability surface changes for a reason, and the reader needs to see
    // everything it touched at once.
    let mut problems: Vec<String> = Vec::new();

    for name in &on_disk {
        if !GUESTS.iter().any(|g| g.name == name) {
            problems.push(format!(
                "{name}.wasm is in guests/out and this table says nothing about it. A module \
                 nobody has stated a verdict for is a capability surface nobody has read: add \
                 it to GUESTS with the verdict it must draw"
            ));
        }
    }

    for g in GUESTS {
        let path = dir.join(format!("{}.wasm", g.name));
        let wasm = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                problems.push(format!(
                    "{}.wasm is missing while {} other module(s) are present. \
                     `guests/build.sh` writes every module it owns or exits non-zero, so this \
                     is a half-finished build directory rather than an unbuilt one",
                    g.name,
                    on_disk.len()
                ));
                continue;
            }
        };

        println!("{}.wasm ({} bytes)", g.name, wasm.len());
        let std_v = match validator::validate(&wasm, Limits::default()) {
            Ok(r) => {
                println!(
                    "    ACCEPTED (Standard): {}",
                    r.manifest().replace('\n', "\n    ")
                );
                vec![]
            }
            Err(vs) => {
                for v in &vs {
                    println!("    REJECTED (Standard) -> {v}");
                }
                render_violations(&vs)
            }
        };

        let mut want_sorted: Vec<String> = g
            .standard
            .iter()
            .map(|(c, s)| format!("{c:?} {s}"))
            .collect();
        want_sorted.sort();
        let mut got_sorted = std_v.clone();
        got_sorted.sort();
        if got_sorted != want_sorted {
            problems.push(format!(
                "{}.wasm on default numeric limits\n     wanted: {}\n        got: {}",
                g.name,
                render(&want_sorted),
                render(&got_sorted)
            ));
        }

        // The plan choice, proven rather than asserted by comment. Free is
        // stricter than Standard in exactly one place — the memory ceiling —
        // so everything Free adds must be V3, and nothing Standard found may
        // disappear.
        let free_v = verdict(
            &wasm,
            Limits {
                module_bytes_compressed: 5242880,
                instance_memory_max: 67108864,
                cpu_ms_per_http_dispatch: 10,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
        );
        for f in &free_v {
            if !std_v.contains(f) && !f.starts_with(&format!("{:?} ", V::V3MemoryShape)) {
                problems.push(format!(
                    "{}.wasm is rejected on restricted numeric limits for `{f}`, which default numeric limits does not \
                     report and which is not a memory-ceiling difference. This test validates \
                     against Standard because that is the ceiling these modules are linked at; \
                     that is only sound while Free differs on memory alone",
                    g.name
                ));
            }
        }
        for s in &std_v {
            if !free_v.contains(s) {
                problems.push(format!(
                    "{}.wasm is rejected on default numeric limits for `{s}` and accepted for it on the \
                     stricter restricted numeric limits, which cannot be right",
                    g.name
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{} module(s) in guests/out do not match the verdict this table records:\n  - {}",
        problems.len(),
        problems.join("\n  - ")
    );
}

/// A module's violations as sortable `"<code> <subject>"` strings. An accepted
/// module yields an empty vector.
fn verdict(wasm: &[u8], limits: Limits) -> Vec<String> {
    validator::validate(wasm, limits).map_or_else(|vs| render_violations(&vs), |_| vec![])
}

fn render_violations(vs: &[validator::Violation]) -> Vec<String> {
    vs.iter()
        .map(|v| format!("{:?} {}", v.code, v.subject.as_deref().unwrap_or("-")))
        .collect()
}

fn render(v: &[String]) -> String {
    if v.is_empty() {
        "ACCEPTED".into()
    } else {
        v.join(", ")
    }
}

/// Pre-init end to end against the guest SDK's own deferral machinery: the pure
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
         {} refusals, seed read {}x, {} ms CPU",
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
