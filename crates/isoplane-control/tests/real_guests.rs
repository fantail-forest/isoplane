//! Built Rust guests through admission and preparation, including hostile modules.

use isoplane_control::validator::{self, ViolationCode as V};

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

/// A guest built with the Rust SDK, prepared as a node prepares it.
#[cfg(target_os = "linux")]
mod prepared {
    use isoplane_control::validator;
    use isoplane_harness::preparation;

    /// Prepares `wasm`, built with the Rust SDK, the way a node prepares a
    /// module that carries its whole application, under the default limits.
    async fn prepare(wasm: &[u8]) -> preparation::Prepared {
        let engine = isoplane_engine::EngineConfig {
            max_instances: 4,
            ..Default::default()
        };
        let initialization = isoplane_prepare::Initialization::new(b"{}".to_vec());
        let request = isoplane_prepare::BuildRequest {
            runtime: isoplane_prepare::RuntimeProfile::RawWasm,
            ..preparation::request(wasm, engine, initialization)
        };
        preparation::prepare_whole_module(&request)
            .await
            .expect("the guest must initialize")
    }

    /// The memory an instance of `wasm` starts with when it is not prepared.
    fn unprepared_memory(prepared: &preparation::Prepared, wasm: &[u8]) -> Vec<u8> {
        prepared
            .engine
            .load_template(wasm)
            .expect("the module must load as submitted")
            .memory_image_arc()
            .to_dense()
    }

    #[tokio::test]
    async fn a_guest_built_with_the_rust_sdk_is_prepared_into_a_template_without_its_initializer() {
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

        let out = prepare(&wasm).await;

        assert_ne!(
            out.memory(),
            unprepared_memory(&out, &wasm),
            "the template must carry what the initializer did, not the input module's memory"
        );
        assert!(
            !out.template.has_initialize(),
            "the template must not run isoplane_initialize again"
        );
    }
}
