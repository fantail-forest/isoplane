//! The `wisp` binary, driven as a user would drive it.

use std::process::Command;

const WISP: &str = env!("CARGO_BIN_EXE_wisp");

/// A core module with the given imports and exports, each carrying the
/// signature the world declares for it; a name the world does not know gets
/// `() -> ()`.
fn module(imports: &[(&str, &str)], exports: &[&str]) -> Vec<u8> {
    module_with(imports, exports, None)
}

/// [`module`] with a declared memory maximum, which is legal on one limits and
/// not on another.
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
        wat.push_str(&format!("  (func (export \"{e}\"){sig} unreachable)\n"));
    }
    wat.push_str(")\n");
    wat::parse_str(&wat).expect("the fixture is well-formed wat")
}

/// A module written to its own temporary directory, which lives as long as
/// the returned guard.
fn write_module(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
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
    let (_dir, p) = write_module(
        "good.wasm",
        &module(
            &[
                ("wisp", "http_fetch"),
                ("wisp", "http_body_read"),
                ("wisp", "clock_now_coarse"),
                ("wisp", "init_hash_seed"),
            ],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);

    assert!(
        out.stdout.contains("CAPABILITY MANIFEST"),
        "the manifest is printed"
    );
    for name in [
        "http_fetch",
        "http_body_read",
        "clock_now_coarse",
        "init_hash_seed",
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
    assert!(
        out.stdout.contains("sensitive"),
        "sensitive imports are marked"
    );
    assert!(
        out.stdout.contains("SO: THIS CODE CAN")
            && out.stdout.contains("make outbound HTTP requests"),
        "the plain-English answer names each capability group"
    );
    assert!(
        out.stdout
            .contains("no threads, no files, no ambient sockets"),
        "what a module cannot do is stated"
    );
    assert!(
        out.stdout
            .contains("✓ every import is a declared capability"),
        "a clean module gets a clean verdict"
    );
}

#[test]
fn doctor_rejects_a_module_that_reaches_for_an_undeclared_capability() {
    let (_dir, p) = write_module(
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
    assert_eq!(out.code, Some(2), "a rejected module must exit 2");
    assert!(
        out.stdout.contains("V1")
            && out.stdout.contains("wasi_snapshot_preview1.fd_write")
            && out.stdout.contains("env.pthread_create"),
        "each undeclared import is named under its rule: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("REJECTED") && out.stdout.contains("no dynamic-linking escape hatch"),
        "the verdict is stated: {}",
        out.stdout
    );
}

/// The whole PAL whitelist is accepted, as the validator accepts it.
#[test]
fn doctor_accepts_the_wasi_imports_the_pal_whitelist_covers() {
    let pal: Vec<(&str, &str)> = wisp_abi::pal::PAL_IMPORTS
        .iter()
        .map(|d| (d.module, d.func))
        .collect();
    let (_dir, p) = write_module(
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
    assert!(
        !out.stdout.contains("REJECTED"),
        "a whitelisted import is not a rejection: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("WASI PAL ADAPTER"),
        "the PAL entries are listed, not dropped: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("wasi:random/random@0.2.0::get-random-bytes"),
        "a PAL entry is named in full: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("delegates to wisp.entropy_fill")
            && out.stdout.contains("draw randomness from the host CSPRNG"),
        "a delegated entry is the capability it names: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("stub: traps when called"),
        "a trap-on-call entry is marked as one: {}",
        out.stdout
    );
    // Of the whole whitelist only `get-random-bytes` reaches a capability with
    // a bullet; a second bullet would mean a trap was counted as one.
    let bullets: Vec<_> = out
        .stdout
        .lines()
        .filter(|l| l.trim_start().starts_with('•'))
        .collect();
    assert_eq!(bullets.len(), 1, "{bullets:?}");
}

#[test]
fn a_wasi_import_outside_the_whitelist_is_still_refused() {
    let (_dir, p) = write_module(
        "offwhitelist.wasm",
        &module(
            &[("wasi:sockets/tcp@0.2.0", "[method]tcp-socket.start-connect")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "a wasi import off the whitelist is rejected: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("wasi:sockets/tcp"),
        "the import is named: {}",
        out.stdout
    );
}

#[test]
fn doctor_notices_a_module_that_cannot_be_activated_at_all() {
    let (_dir, p) = write_module(
        "noexports.wasm",
        &module(&[("wisp", "http_fetch")], &["memory"]),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "a module the host cannot enter is rejected"
    );
    assert!(
        out.stdout
            .contains("required export `wisp_handle` is missing"),
        "the missing export is named: {}",
        out.stdout
    );
}

#[test]
fn doctor_has_a_machine_readable_form() {
    let (_dir, p) = write_module(
        "json.wasm",
        &module(
            &[("wisp", "http_fetch")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--json", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "a clean module exits 0");
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(v["rejected"], false, "the verdict is in the JSON");
    assert_eq!(
        (
            &v["capabilities"][0]["name"],
            &v["capabilities"][0]["group"],
            &v["capabilities"][0]["kind"]
        ),
        (&"http_fetch".into(), &"http".into(), &"future".into()),
        "each capability carries its name, group and kind"
    );
    assert_eq!(
        v["blake3"].as_str().unwrap().len(),
        64,
        "the hash is the full blake3 hex"
    );
}

#[test]
fn doctor_says_so_when_a_module_holds_no_io_capability_at_all() {
    let (_dir, p) = write_module(
        "pure.wasm",
        &module(
            &[("wisp", "log_emit"), ("wisp", "resp_new")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        ),
    );
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "a pure module is accepted");
    assert!(
        out.stdout.contains("it holds no I/O capability at all"),
        "and says it holds no I/O capability: {}",
        out.stdout
    );
}

/// A module the manifest passes and the validator refuses on the smaller limits.
#[cfg(feature = "control")]
#[test]
fn doctor_fails_a_module_the_manifest_alone_would_pass() {
    let m = module_with(
        &[("wisp", "http_fetch")],
        &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
        Some(2048),
    );
    let (_dir, p) = write_module("bigmem.wasm", &m);

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
    assert!(
        bad.stdout
            .contains("✓ every import is a declared capability"),
        "the manifest alone would have passed it: {}",
        bad.stdout
    );
    assert!(
        bad.stdout.contains("V3MemoryShape"),
        "the validator names the rule: {}",
        bad.stdout
    );
    assert!(bad.stdout.contains("numeric limits from"), "{}", bad.stdout);
}

#[cfg(feature = "control")]
#[test]
fn doctor_takes_numeric_limits_from_the_app_manifest() {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let dir = tmp.path().to_path_buf();
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
    assert_eq!(
        out.code,
        Some(2),
        "the app's limits refuses the module: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("numeric limits from") && out.stdout.contains("wisp.toml"),
        "the limits and where it came from are stated: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("V3MemoryShape"),
        "the validator names the rule: {}",
        out.stdout
    );
}

#[test]
fn a_garbage_module_is_an_error_not_a_panic() {
    let (_dir, p) = write_module("garbage.wasm", b"this is not wasm");
    let out = run(&["doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(1), "garbage is a command failure");
    assert!(
        !out.stderr.contains("panicked"),
        "and not a panic: {}",
        out.stderr
    );
}

#[test]
fn every_documented_subcommand_exists() {
    let help = run(&["--help"]).stdout;
    for cmd in ["dev", "doctor"] {
        assert!(help.contains(cmd), "`wisp {cmd}` is missing from --help");
    }
}

#[cfg(feature = "control")]
#[test]
fn a_numeric_memory_override_keeps_other_admission_limits_and_capability_checks() {
    let (_directory, path) = write_module(
        "bounded-large.wasm",
        &module_with(&[], wisp_abi::imports::exports::REQUIRED, Some(8192)),
    );
    let out = run(&[
        "--json",
        "doctor",
        path.to_str().unwrap(),
        "--guest-memory-bytes",
        "536870912",
    ]);
    assert_eq!(
        out.code,
        Some(0),
        "a bounded explicit memory override must pass: {} {}",
        out.stdout,
        out.stderr
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(verdict["guest_memory_bytes"], 536870912_u64);
    assert_eq!(
        verdict["limits"]["module_bytes_compressed"], 20971520_u64,
        "a memory flag must preserve the application size allowance"
    );
    let (_directory, path) = write_module(
        "hostile-large.wasm",
        &module_with(
            &[("outside", "io")],
            wisp_abi::imports::exports::REQUIRED,
            Some(8192),
        ),
    );
    let out = run(&[
        "--json",
        "doctor",
        path.to_str().unwrap(),
        "--guest-memory-bytes",
        "536870912",
    ]);
    assert_eq!(
        out.code,
        Some(2),
        "a memory flag cannot grant another capability: {} {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("V1UndeclaredImport"),
        "the actual validator must explain the rejection: {}",
        out.stdout
    );
}
