//! Built Rust guests through admission and preparation, including hostile modules.

use isoplane_control::validator::{self, ViolationCode as V};
use isoplane_prepare::preinit::{PreInitRequest, PreInitializer};

/// The verdict admission under the default limits must reach for each module in
/// the build script's manifest, as `(code, subject)` pairs; empty means the
/// module is accepted.
const VERDICTS: &[(&str, &[(V, &str)])] = &[
    ("hello", &[]),
    ("quotes-api", &[]),
    ("misbehaver", &[]),
    (
        "undeclared-import",
        &[(V::V1UndeclaredImport, "isoplane.fs_open")],
    ),
    (
        "foreign-module",
        &[(V::V1UndeclaredImport, "env.__wasi_fd_write")],
    ),
];

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
        let std_v = verdict(&wasm, isoplane_abi::config::GuestMemoryBytes::default());
        let mut want_sorted: Vec<String> = want.iter().map(|(c, s)| format!("{c:?} {s}")).collect();
        want_sorted.sort();
        let mut got_sorted = std_v.clone();
        got_sorted.sort();
        if got_sorted != want_sorted {
            problems.push(format!(
                "{name}.wasm on isoplane_abi::config::GuestMemoryBytes::default()\n     wanted: {}\n        got: {}",
                render(&want_sorted),
                render(&got_sorted)
            ));
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
fn verdict(wasm: &[u8], memory: isoplane_abi::config::GuestMemoryBytes) -> Vec<String> {
    validator::validate_with_limits(wasm, memory, None, &Default::default()).map_or_else(
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
fn a_guest_built_with_the_rust_sdk_is_prepared_into_a_template_without_its_initializer() {
    let wasm = isoplane_fixtures::rust("quotes-api");
    let report = validator::validate_with_limits(
        &wasm,
        isoplane_abi::config::GuestMemoryBytes::default(),
        None,
        &Default::default(),
    )
    .expect("quotes-api must validate");
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
        .expect("pre-init must succeed");

    assert_ne!(out.wasm, wasm, "the template must not be the input module");
    assert!(
        !exports_init(&out.wasm),
        "isoplane_initialize must be stripped from the template"
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
