//! The `wisp` binary, driven as a user would drive it.
//!
//! `wisp doctor` gets the most attention here because it is where the claim
//! that "what can this code do?" is a static question meets a user — and a
//! claim like that is only worth what its user-visible answer is worth.

use std::process::Command;

const WISP: &str = env!("CARGO_BIN_EXE_wisp");

/// A minimal core-wasm module: the given imports, one memory, and the given
/// exports — each carrying the signature the world declares for it.
///
/// The signatures are read out of `wisp_abi` rather than written here. They
/// used to be `() -> ()` for everything, which was fine while `doctor` only
/// matched names; the validator it now runs checks types (V1SignatureMismatch),
/// and a fixture with hand-written types would only be testing that two copies
/// of the world had not yet drifted. A name the world does not know gets
/// `() -> ()` — in those cases the name is what is under test.
fn module(imports: &[(&str, &str)], exports: &[&str]) -> Vec<u8> {
    module_with(imports, exports, None)
}

/// [`module`], with a declared memory maximum in pages. The maximum is the one
/// thing in a module that is legal on one limits and not on another, which makes
/// it the fixture for limits-sensitivity.
fn module_with(imports: &[(&str, &str)], exports: &[&str], max_pages: Option<u32>) -> Vec<u8> {
    use wisp_abi::imports::{Sig, Ty};

    fn ty(t: Ty) -> &'static str {
        match t {
            Ty::I32 => "i32",
            Ty::I64 => "i64",
        }
    }
    fn signature(s: Option<Sig>) -> String {
        let Some(s) = s else { return String::new() };
        let mut out = String::new();
        if !s.params.is_empty() {
            out.push_str(" (param");
            for p in s.params {
                out.push(' ');
                out.push_str(ty(*p));
            }
            out.push(')');
        }
        if !s.results.is_empty() {
            out.push_str(" (result");
            for r in s.results {
                out.push(' ');
                out.push_str(ty(*r));
            }
            out.push(')');
        }
        out
    }

    let mut wat = String::from("(module\n");
    for (module_, field) in imports {
        // Module-aware, the way the classifier is: `kv_get` is a signature in
        // the `wisp` table and `get-random-bytes` is one in the PAL table, and
        // the validator checks both.
        let declared = if *module_ == wisp_abi::imports::HOST_MODULE {
            wisp_abi::imports::lookup(field).map(|d| d.sig)
        } else {
            wisp_abi::pal::lookup(module_, field).and_then(|d| d.sig)
        };
        let sig = signature(declared);
        wat.push_str(&format!(
            "  (import \"{module_}\" \"{field}\" (func{sig}))\n"
        ));
    }
    let limits = match max_pages {
        Some(max) => format!("1 {max}"),
        None => "1".into(),
    };
    let export = if exports.contains(&"memory") {
        " (export \"memory\")"
    } else {
        ""
    };
    wat.push_str(&format!("  (memory{export} {limits})\n"));
    for e in exports.iter().filter(|e| **e != "memory") {
        let sig = signature(wisp_abi::imports::exports::signature(e));
        // `unreachable` is well-typed under any result type, so one body
        // serves every export.
        wat.push_str(&format!("  (func (export \"{e}\"){sig} unreachable)\n"));
    }
    wat.push_str(")\n");
    wat::parse_str(&wat).expect("the fixture is well-formed wat")
}

fn write_module(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wisp-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let o = Command::new(WISP)
        .args(args)
        .output()
        .expect("running the wisp binary");
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

#[test]
fn doctor_prints_the_capability_manifest_with_its_policy_bits() {
    let p = write_module(
        "good.wasm",
        &module(
            &[
                ("wisp", "http_fetch"),
                ("wisp", "http_body_read"),
                ("wisp", "init_hash_seed"),
                ("wisp", "clock_now_coarse"),
            ],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);

    // The manifest itself.
    assert!(out.stdout.contains("CAPABILITY MANIFEST"));
    for name in [
        "http_fetch",
        "http_body_read",
        "init_hash_seed",
        "clock_now_coarse",
    ] {
        assert!(
            out.stdout.contains(name),
            "missing {name} from the manifest"
        );
    }
    assert!(
        out.stdout.contains("http_body_read") && out.stdout.contains("future"),
        "future-returning imports remain visible"
    );
    assert!(out.stdout.contains("sensitive"));
    // The plain-English answer, which is the actual point of the command.
    assert!(out.stdout.contains("SO: THIS CODE CAN"));
    assert!(out.stdout.contains("make outbound HTTP requests"));
    assert!(out
        .stdout
        .contains("no threads, no files, no ambient sockets"));
    assert!(out
        .stdout
        .contains("✓ every import is a declared capability"));
}

#[test]
fn doctor_rejects_a_module_that_reaches_for_an_undeclared_capability() {
    // V1. The two undeclared imports here are the two things a real
    // module would most plausibly try to smuggle in.
    let p = write_module(
        "bad.wasm",
        &module(
            &[
                ("wisp", "http_fetch"),
                ("wasi_snapshot_preview1", "fd_write"),
                ("env", "pthread_create"),
            ],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2), "a rejected module must exit non-zero");
    assert!(out.stdout.contains("V1"));
    assert!(out.stdout.contains("wasi_snapshot_preview1.fd_write"));
    assert!(out.stdout.contains("env.pthread_create"));
    assert!(out.stdout.contains("REJECTED"));
    assert!(out.stdout.contains("no dynamic-linking escape hatch"));
}

/// A C# guest linked without the PAL object file imports `wasi:*` directly,
/// and the deploy-time composition satisfies every one of those from the
/// enumerated whitelist. The module is valid — `wisp_control::validate`
/// accepts it — so `doctor` must too.
///
/// This is the regression for a build-breaking bug: `doctor` classified any
/// import outside module `wisp` as undeclared, its verdict was OR-ed into the
/// exit code, and the SDK's post-link MSBuild target then failed the build of
/// a guest that would have deployed. Two checkers that can disagree is the
/// defect; the fix is that both read `wisp_abi::pal`.
#[test]
fn doctor_accepts_the_wasi_imports_the_pal_whitelist_covers() {
    let pal: Vec<(&str, &str)> = wisp_abi::pal::PAL_IMPORTS
        .iter()
        .map(|d| (d.module, d.func))
        .collect();
    let p = write_module(
        "pal.wasm",
        &module(&pal, &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"]),
    );

    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(0),
        "the whole whitelist must be accepted:\n{}",
        out.stdout
    );
    assert!(!out.stdout.contains("REJECTED"), "{}", out.stdout);

    // Present in the manifest rather than silently dropped from it: the claim
    // this command exists to make is that the list is complete.
    assert!(out.stdout.contains("WASI PAL ADAPTER"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("wasi:random/random@0.2.0::get-random-bytes"),
        "{}",
        out.stdout
    );
    // A delegated entry IS the capability it names, so the plain-English
    // answer has to include it even though nothing imports `wisp.entropy_fill`.
    assert!(
        out.stdout.contains("delegates to wisp.entropy_fill"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("draw randomness from the host CSPRNG"),
        "{}",
        out.stdout
    );
    // A stubbed one reaches nothing, and must not be read as a capability.
    // The whole whitelist is imported here and exactly one bullet comes out of
    // it: `get-random-bytes` delegates to `entropy_fill`, `environment` is
    // `Delegate(Nothing)`, the clock's group has no bullet, and every other
    // entry traps. Anything more would mean a trap had been counted as a
    // capability.
    assert!(
        out.stdout.contains("stub: traps when called"),
        "{}",
        out.stdout
    );
    let bullets: Vec<_> = out
        .stdout
        .lines()
        .filter(|l| l.trim_start().starts_with('•'))
        .collect();
    assert_eq!(bullets.len(), 1, "{bullets:?}");
}

/// And a `wasi:*` import that is NOT on the whitelist is still a rejection —
/// the fix must not have become "anything spelled `wasi:` is fine".
#[test]
fn a_wasi_import_outside_the_whitelist_is_still_refused() {
    let p = write_module(
        "offwhitelist.wasm",
        &module(
            &[("wasi:sockets/tcp@0.2.0", "[method]tcp-socket.start-connect")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2), "{}", out.stdout);
    assert!(out.stdout.contains("wasi:sockets/tcp"), "{}", out.stdout);
}

#[test]
fn doctor_notices_a_module_that_cannot_be_activated_at_all() {
    let p = write_module(
        "noexports.wasm",
        &module(&[("wisp", "http_fetch")], &["memory"]),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2));
    assert!(
        out.stdout
            .contains("required export `wisp_handle` is missing"),
        "{}",
        out.stdout
    );
}

#[test]
fn doctor_has_a_machine_readable_form() {
    let p = write_module(
        "json.wasm",
        &module(
            &[("wisp", "http_fetch")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--json", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(v["rejected"], false);
    assert_eq!(v["capabilities"][0]["name"], "http_fetch");
    assert_eq!(v["capabilities"][0]["group"], "http");
    assert_eq!(v["capabilities"][0]["kind"], "future");
    assert!(v["blake3"].as_str().unwrap().len() == 64);
}

#[test]
fn doctor_says_so_when_a_module_holds_no_io_capability_at_all() {
    let p = write_module(
        "pure.wasm",
        &module(
            &[("wisp", "log_emit"), ("wisp", "resp_new")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains("it holds no I/O capability at all"));
}

/// The derived manifest and the real validator answer different questions, and
/// `doctor`'s exit code has to be the union: this module's imports and exports
/// are all declared and correctly typed — the manifest says yes — and it is
/// still refused on smaller, for a reason only the validator knows.
#[cfg(feature = "control")]
#[test]
fn doctor_fails_a_module_the_manifest_alone_would_pass() {
    let m = module_with(
        &[("wisp", "http_fetch")],
        &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        Some(2048),
    );
    let p = write_module("bigmem.wasm", &m);

    let ok = run(&[
        "--no-color",
        "doctor",
        p.to_str().unwrap(),
        "--guest-memory-bytes",
        "134217728",
    ]);
    assert_eq!(
        ok.code,
        Some(0),
        "128 MiB is the configured ceiling: {}",
        ok.stdout
    );
    assert!(ok.stdout.contains("numeric limits from"), "{}", ok.stdout);

    let bad = run(&[
        "--no-color",
        "doctor",
        p.to_str().unwrap(),
        "--guest-memory-bytes",
        "67108864",
    ]);
    assert_eq!(
        bad.code,
        Some(2),
        "the same module is over the configured ceiling: {}",
        bad.stdout
    );
    // The manifest, which is all `doctor` used to consult, is still happy.
    assert!(bad
        .stdout
        .contains("✓ every import is a declared capability"));
    assert!(bad.stdout.contains("V3MemoryShape"), "{}", bad.stdout);
    assert!(bad.stdout.contains("numeric limits from"), "{}", bad.stdout);
}

/// And the limits is not something the user has to remember to pass: it comes
/// from the app's own `wisp.toml`, found from the module outwards. A `doctor`
/// that judged against `Limits::default()` regardless would pass this module and
/// `deploy` would then refuse it.
#[cfg(feature = "control")]
#[test]
fn doctor_takes_numeric_limits_from_the_app_manifest() {
    let dir = std::env::temp_dir().join(format!("wisp-cli-freeapp-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("out")).unwrap();
    std::fs::write(
        dir.join("wisp.toml"),
        "name = \"pinched\"\nlimits = { module_bytes_compressed = 5242880, instance_memory_max = 67108864, cpu_ms_per_http_dispatch = 10, subrequests_per_dispatch = 20, pending_futures_per_dispatch = 6 }\n",
    )
    .unwrap();
    let p = dir.join("out").join("bigmem.wasm");
    std::fs::write(
        &p,
        module_with(
            &[("wisp", "http_fetch")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(2048),
        ),
    )
    .unwrap();

    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2), "{}", out.stdout);
    assert!(out.stdout.contains("numeric limits from"), "{}", out.stdout);
    assert!(out.stdout.contains("wisp.toml"), "{}", out.stdout);
    assert!(out.stdout.contains("V3MemoryShape"), "{}", out.stdout);
}

#[test]
fn a_garbage_module_is_an_error_not_a_panic() {
    let p = write_module("garbage.wasm", b"this is not wasm");
    let out = run(&["doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(1));
    assert!(!out.stderr.contains("panicked"), "stderr: {}", out.stderr);
}

#[test]
fn every_documented_subcommand_exists() {
    let help = run(&["--help"]).stdout;
    for cmd in ["dev", "doctor"] {
        assert!(help.contains(cmd), "`wisp {cmd}` is missing from --help");
    }
}
