//! `wisp doctor`: the capability manifest and its machine-readable form.

mod common;

use common::run;

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
    let bullets: Vec<_> = out
        .stdout
        .lines()
        .filter(|line| line.trim_start().starts_with('•'))
        .collect();
    assert_eq!(
        bullets.len(),
        2,
        "only entropy and outbound sockets are reachable through the PAL: {bullets:?}"
    );
    assert!(
        out.stdout
            .contains("open outbound TCP connections and resolve DNS names"),
        "the PAL socket capability must describe outbound TCP and DNS: {}",
        out.stdout
    );
}

#[test]
fn a_wasi_import_outside_the_whitelist_is_still_refused() {
    let (_dir, p) = write_module(
        "offwhitelist.wasm",
        &module(
            &[(
                "wasi:sockets/tcp@0.2.0",
                "[method]tcp-socket.unknown-operation",
            )],
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

#[cfg(feature = "control")]
#[test]
fn doctor_reports_and_enforces_the_default_memory_ceiling() {
    let (_dir, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(8192),
        ),
    );
    let out = run(&["--json", "doctor", path.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "the default must reject a 512 MiB memory declaration"
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        verdict["limits"]["module_bytes_compressed"], 20971520,
        "the default plan must remain Standard"
    );
    assert_eq!(
        verdict["guest_memory_bytes"], 134217728,
        "JSON must report the effective default memory ceiling"
    );
    assert_eq!(
        verdict["guest_memory_from"], "limits",
        "JSON must distinguish the configured ceiling from an override"
    );
}

#[cfg(feature = "control")]
#[test]
fn doctor_admits_an_explicit_memory_override_without_changing_other_limits() {
    let (_dir, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(8192),
        ),
    );
    for format in ["--json", "--no-color"] {
        let out = run(&[
            format,
            "doctor",
            path.to_str().unwrap(),
            "--guest-memory-bytes",
            "536870912",
        ]);
        assert_eq!(
            out.code,
            Some(0),
            "the explicit ceiling must admit the bounded module: {}",
            out.stdout
        );
        if format == "--json" {
            let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
            assert_eq!(
                verdict["limits"]["module_bytes_compressed"], 20971520,
                "the memory override cannot select Enterprise"
            );
            assert_eq!(
                verdict["guest_memory_bytes"], 536870912_u64,
                "JSON must report the applied byte ceiling"
            );
            assert_eq!(
                verdict["guest_memory_from"], "--guest-memory-bytes",
                "JSON must identify the override source"
            );
        } else {
            assert!(
                out.stdout.contains("numeric limits from")
                    && out
                        .stdout
                        .contains("guest memory ceiling: 536870912 bytes (--guest-memory-bytes)"),
                "text output must explain the independent memory ceiling: {}",
                out.stdout
            );
        }
    }
}

#[cfg(feature = "control")]
#[test]
fn a_doctor_memory_override_requires_a_declared_module_maximum() {
    let (_dir, path) = write_module(
        "unbounded.wasm",
        &module(&[], &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"]),
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
        "an override cannot admit unbounded guest memory"
    );
    assert!(
        out.stdout.contains("V3MemoryShape"),
        "the rejection must name the memory rule: {}",
        out.stdout
    );
}

#[cfg(feature = "control")]
#[test]
fn a_doctor_memory_override_does_not_admit_hostile_imports() {
    let (_dir, path) = write_module(
        "hostile.wasm",
        &module_with(
            &[("env", "pthread_create")],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
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
        "a larger memory ceiling cannot grant an undeclared capability"
    );
    assert!(
        out.stdout.contains("V1UndeclaredImport") && out.stdout.contains("pthread_create"),
        "the hostile import must remain visible in the validator rejection: {}",
        out.stdout
    );
}

#[test]
fn invalid_doctor_memory_budgets_name_the_supported_byte_constraints() {
    for bytes in ["0", "1", "134217729", "4295032832", "invalid"] {
        let out = run(&["doctor", "unused.wasm", "--guest-memory-bytes", bytes]);
        assert_eq!(
            out.code,
            Some(2),
            "invalid memory policy must fail argument parsing"
        );
        assert!(
            out.stderr.contains("65536") && out.stderr.contains("4294967296"),
            "an invalid ceiling must name its required alignment and maximum: {}",
            out.stderr
        );
    }
}

#[cfg(feature = "control")]
#[test]
fn doctor_inherits_the_module_manifests_memory_ceiling_and_reports_its_source() {
    let (directory, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(8192),
        ),
    );
    let manifest = directory.path().join("wisp.toml");
    std::fs::write(
        &manifest,
        "name='large'\nlimits = { module_bytes_compressed = 5242880, instance_memory_max = 67108864, cpu_ms_per_http_dispatch = 10, subrequests_per_dispatch = 20, pending_futures_per_dispatch = 6 }\nguest_memory_bytes=536870912\n",
    )
    .unwrap();
    let out = run(&["--json", "doctor", path.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(0),
        "a limits selection flag must not discard the manifest memory ceiling: {}",
        out.stdout
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        verdict["limits"]["module_bytes_compressed"], 5242880,
        "the limits flag must remain independent of manifest memory"
    );
    assert_eq!(
        verdict["guest_memory_bytes"], 536870912_u64,
        "doctor must apply manifest memory"
    );
    assert_eq!(
        verdict["guest_memory_from"],
        format!("guest_memory_bytes in {}", manifest.display()),
        "the verdict must name the memory setting's actual manifest"
    );
    let text = run(&["--no-color", "doctor", path.to_str().unwrap()]);
    assert_eq!(
        text.code,
        Some(0),
        "the inherited policy must also pass text-mode validation"
    );
    assert!(
        text.stdout.contains(&format!(
            "guest memory ceiling: 536870912 bytes (guest_memory_bytes in {}",
            manifest.display()
        )),
        "text output must identify the inherited memory ceiling: {}",
        text.stdout
    );
}

#[cfg(feature = "control")]
#[test]
fn an_explicit_doctor_memory_flag_overrides_the_manifest() {
    let (directory, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(8192),
        ),
    );
    std::fs::write(
        directory.path().join("wisp.toml"),
        "name='large'\nlimits = { module_bytes_compressed = 20971520, instance_memory_max = 134217728, cpu_ms_per_http_dispatch = 30000, subrequests_per_dispatch = 200, pending_futures_per_dispatch = 64 }\nguest_memory_bytes=536870912\n",
    )
    .unwrap();
    let out = run(&[
        "--json",
        "doctor",
        path.to_str().unwrap(),
        "--guest-memory-bytes",
        "134217728",
    ]);
    assert_eq!(
        out.code,
        Some(2),
        "the smaller flag must override the otherwise admitting manifest"
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        verdict["guest_memory_bytes"], 134217728,
        "the effective ceiling must come from the flag"
    );
    assert_eq!(
        verdict["guest_memory_from"], "--guest-memory-bytes",
        "the memory source must identify the winning flag"
    );
    assert_eq!(
        verdict["limits"]["module_bytes_compressed"], 20971520,
        "a memory flag must not change the manifest limits"
    );
}

#[cfg(feature = "control")]
#[test]
fn a_nearer_manifest_without_memory_does_not_inherit_an_enclosing_apps_budget() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("wisp.toml"),
        "name='outer'\nguest_memory_bytes=536870912\n",
    )
    .unwrap();
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(
        nested.join("wisp.toml"),
        "name='inner'\nlimits = { module_bytes_compressed = 20971520, instance_memory_max = 134217728, cpu_ms_per_http_dispatch = 30000, subrequests_per_dispatch = 200, pending_futures_per_dispatch = 64 }\n",
    )
    .unwrap();
    let path = nested.join("large.wasm");
    std::fs::write(
        &path,
        module_with(
            &[],
            &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"],
            Some(8192),
        ),
    )
    .unwrap();
    let out = run(&["--json", "doctor", path.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "an outer application's ceiling cannot authorize the inner module"
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        verdict["guest_memory_from"], "limits",
        "an absent setting must use the selected configured ceiling"
    );
    assert_eq!(
        verdict["guest_memory_bytes"], 134217728,
        "the inner app must retain its default memory ceiling"
    );
}
