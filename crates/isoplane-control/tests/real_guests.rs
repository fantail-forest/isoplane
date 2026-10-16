//! Validation and initializer replay against prepared guest modules.


use isoplane_abi::config::{InitSandbox, Limits};
use isoplane_control::preinit::{PreInitRequest, PreInitializer};
use isoplane_control::validator::{self, ViolationCode as V};

/// The verdict `validate(.., default numeric limits)` must reach for each module in
/// the build script's manifest, as `(code, subject)` pairs; empty means the
/// module is accepted.
const VERDICTS: &[(&str, &[(V, &str)])] = &[
    ("hello", &[]),
    ("quotes-api", &[]),
    ("misbehaver", &[]),
    ("impure-cctor", &[]),
    (
        "undeclared-import",
        &[(V::V1UndeclaredImport, "isoplane.fs_open")],
    ),
    (
        "foreign-module",
        &[(V::V1UndeclaredImport, "env.__wasi_fd_write")],
    ),
];

/// `default numeric limits` because it is the ceiling the guests are linked at
/// (`guests/.cargo/config.toml` passes its `instance_memory_max`). The plans
/// differ on memory alone, which the test holds to: everything `restricted numeric limits`
/// adds must be `V3MemoryShape`, and nothing Standard finds may vanish.
#[test]
fn every_module_build_sh_writes_draws_the_verdict_recorded_for_it() {
    let built = isoplane_fixtures::rust_manifest();
    let mut problems: Vec<String> = Vec::new();

    for name in &built {
        if !VERDICTS.iter().any(|(n, _)| n == name) {
            problems.push(format!(
                "guests/build.sh writes {name}.wasm and VERDICTS records nothing for it; a module \
                 nobody has stated a verdict for is a capability surface nobody has read"
            ));
        }
    }

    for (name, want) in VERDICTS {
        if !built.iter().any(|b| b == name) {
            problems.push(format!(
                "VERDICTS records {name}.wasm and guests/build.sh no longer writes it"
            ));
            continue;
        }
        let wasm = isoplane_fixtures::rust(name);
        let std_v = verdict(&wasm, Limits::default());
        let mut want_sorted: Vec<String> = want.iter().map(|(c, s)| format!("{c:?} {s}")).collect();
        want_sorted.sort();
        let mut got_sorted = std_v.clone();
        got_sorted.sort();
        if got_sorted != want_sorted {
            problems.push(format!(
                "{name}.wasm on default numeric limits\n     wanted: {}\n        got: {}",
                render(&want_sorted),
                render(&got_sorted)
            ));
        }

        let free_v = verdict(
            &wasm,
            Limits {
                module_bytes_compressed: 5242880,
                package_bytes: 52428800,
                instance_memory_max: 134217728,
                occupancy_ms_per_dispatch: 100,
                subrequests_per_dispatch: 20,
                pending_futures_per_dispatch: 6,
            },
        );
        for f in &free_v {
            if !std_v.contains(f) && !f.starts_with(&format!("{:?} ", V::V3MemoryShape)) {
                problems.push(format!(
                    "{name}.wasm is rejected on restricted numeric limits for `{f}`, which default numeric limits does \
                     not report and which is not a memory-ceiling difference"
                ));
            }
        }
        for s in &std_v {
            if !free_v.contains(s) {
                problems.push(format!(
                    "{name}.wasm is rejected on default numeric limits for `{s}` and accepted for it on \
                     the stricter restricted numeric limits"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{} module(s) in .build/wasm32/rust-guests do not match the verdict recorded for them:\n  - {}",
        problems.len(),
        problems.join("\n  - ")
    );
}

/// A module's violations as sortable `"<code> <subject>"` strings; an accepted
/// module yields none.
fn verdict(wasm: &[u8], limits: Limits) -> Vec<String> {
    validator::validate(wasm, limits).map_or_else(
        |vs| {
            vs.iter()
                .map(|v| format!("{:?} {}", v.code, v.subject.as_deref().unwrap_or("-")))
                .collect()
        },
        |_| vec![],
    )
}

fn render(v: &[String]) -> String {
    if v.is_empty() {
        "ACCEPTED".into()
    } else {
        v.join(", ")
    }
}

#[test]
fn the_impure_cctor_guest_defers_and_converges() {
    let wasm = isoplane_fixtures::rust("impure-cctor");
    let report = validator::validate(&wasm, Limits::default()).expect("impure-cctor must validate");
    assert!(
        report.has_initialize(),
        "the guest SDK must export isoplane_initialize"
    );

    let req = PreInitRequest {
        module_hash: isoplane_engine::ModuleHash::of(&wasm),
        init_blob: Vec::new(),
    };
    let out = PreInitializer::new()
        .run(&wasm, &req)
        .expect("pre-init must converge");

    assert_ne!(
        out.template_wasm, wasm,
        "the template must not be the input module"
    );
    assert!(
        !exports_init(&out.template_wasm),
        "isoplane_initialize must be stripped from the template"
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
    assert_eq!(
        out.deferred_reasons[0].1, "clock_now_coarse",
        "the deferral reason must name the capability that caused it"
    );
    assert_eq!(
        out.capability_refusals, 1,
        "the impure unit is refused once, on the pass that discovers it"
    );
}

fn exports_init(wasm: &[u8]) -> bool {
    wasmparser::Parser::new(0).parse_all(wasm).any(|p| match p {
        Ok(wasmparser::Payload::ExportSection(r)) => r
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.name == isoplane_abi::imports::exports::INITIALIZE),
        _ => false,
    })
}
