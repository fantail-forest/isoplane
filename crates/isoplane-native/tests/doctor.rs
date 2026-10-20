//! `isoplane doctor`: the capability manifest and its machine-readable form.

mod common;

use common::run;
use isoplane_abi::config::Limits;

/// A core module with the given imports and exports, each carrying the
/// signature the world declares for it; a name the world does not know gets
/// `() -> ()`.
fn module(imports: &[(&str, &str)], exports: &[&str]) -> Vec<u8> {
    module_with(imports, exports, None)
}

/// [`module`] with a declared memory maximum, which is legal on one memory budget and
/// not on another.
fn module_with(imports: &[(&str, &str)], exports: &[&str], max_pages: Option<u32>) -> Vec<u8> {
    use isoplane_abi::imports::{Sig, Ty};

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
        let declared = if *module_ == isoplane_abi::imports::HOST_MODULE {
            isoplane_abi::imports::lookup(field).map(|d| d.sig)
        } else {
            isoplane_abi::pal::lookup(module_, field).and_then(|d| d.sig)
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
        let sig = signature(isoplane_abi::imports::exports::signature(e));
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
                ("isoplane", "http_fetch"),
                ("isoplane", "http_body_read"),
                ("isoplane", "clock_now_coarse"),
                ("isoplane", "init_hash_seed"),
            ],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
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
        out.stdout.contains("sensitive") && out.stdout.contains("init_hash_seed"),
        "sensitive imports are marked"
    );
    assert!(
        out.stdout.contains("SO: THIS CODE CAN")
            && out.stdout.contains("make outbound HTTP requests"),
        "the plain-English answer names each capability group"
    );
    assert!(
        out.stdout.contains("✓ no violations"),
        "a clean module gets a clean verdict"
    );
}

#[test]
fn doctor_rejects_a_module_that_reaches_for_an_undeclared_capability() {
    let (_dir, p) = write_module(
        "bad.wasm",
        &module(
            &[
                ("isoplane", "http_fetch"),
                ("wasi_snapshot_preview1", "fd_write"),
                ("env", "pthread_create"),
            ],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2), "a rejected module must exit 2");
    assert!(
        out.stdout.contains("V1")
            && out.stdout.contains("wasi_snapshot_preview1.fd_write")
            && out.stdout.contains("env.pthread_create"),
        "each undeclared import is named under its rule: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("REJECTED"),
        "the verdict is stated: {}",
        out.stdout
    );
}

/// The whole PAL whitelist is accepted, as the validator accepts it.
#[test]
fn doctor_accepts_the_wasi_imports_the_pal_whitelist_covers() {
    let pal: Vec<(&str, &str)> = isoplane_abi::pal::PAL_IMPORTS
        .iter()
        .map(|d| (d.module, d.func))
        .collect();
    let (_dir, p) = write_module(
        "pal.wasm",
        &module(
            &pal,
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );

    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
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
        out.stdout.contains("delegates to isoplane.entropy_fill")
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
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
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
        &module(&[("isoplane", "http_fetch")], &["memory"]),
    );
    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "a module the host cannot enter is rejected"
    );
    assert!(
        out.stdout
            .contains("required guest export `isoplane_handle` is missing"),
        "the missing export is named: {}",
        out.stdout
    );
}

#[test]
fn doctor_has_a_machine_readable_form() {
    let (_dir, p) = write_module(
        "json.wasm",
        &module(
            &[("isoplane", "http_fetch")],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&["doctor", "--json", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "a clean module exits 0");
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(v["rejected"], false, "the verdict is in the JSON");
    assert_eq!(
        (
            &v["capabilities"][0]["name"],
            &v["capabilities"][0]["group"],
            &v["capabilities"][0]["kind"]
        ),
        (
            &"http_fetch".into(),
            &"http".into(),
            &"future".into()
        ),
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
            &[("isoplane", "log_emit"), ("isoplane", "resp_new")],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&["doctor", "--no-color", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0), "a pure module is accepted");
    assert!(
        out.stdout.contains("it holds no I/O capability at all"),
        "and says it holds no I/O capability: {}",
        out.stdout
    );
}

/// A module the manifest passes and the validator refuses on a smaller memory budget.
#[test]
fn doctor_fails_a_module_the_manifest_alone_would_pass() {
    let m = module_with(
        &[("isoplane", "http_fetch")],
        &[
            "isoplane_alloc",
            "isoplane_handle",
            "isoplane_wake",
            "memory",
        ],
        Some(pages(Limits {
            instances_per_application: 16,
            module_bytes_compressed: 52428800,
            package_bytes: 52428800,
            instance_memory_max: 536870912,
            occupancy_ms_per_dispatch: 300000,
            subrequests_per_dispatch: 10000,
            pending_futures_per_dispatch: 4096,
        })),
    );
    let (_dir, p) = write_module("bigmem.wasm", &m);

    let ok = run(&[
        "doctor",
        "--no-color",
        p.to_str().unwrap(),
        "--guest-memory-bytes",
        "536870912",
    ]);
    assert_eq!(
        ok.code,
        Some(0),
        "the Enterprise ceiling admits its own maximum: {}",
        ok.stdout
    );
    assert!(ok.stdout.contains("numeric limits"), "{}", ok.stdout);

    let bad = run(&[
        "doctor",
        "--no-color",
        p.to_str().unwrap(),
        "--guest-memory-bytes",
        "134217728",
    ]);
    assert_eq!(
        bad.code,
        Some(2),
        "the same module is over the Standard ceiling: {}",
        bad.stdout
    );
    assert!(
        bad.stdout.contains("✓ isoplane_handle") && !bad.stdout.contains("V1MissingExport"),
        "the manifest alone would have passed it: {}",
        bad.stdout
    );
    assert!(
        bad.stdout.contains("V3MemoryShape"),
        "the validator names the rule: {}",
        bad.stdout
    );
    assert!(bad.stdout.contains("numeric limits"), "{}", bad.stdout);
}

/// The wasm pages in a numeric budget's instance memory ceiling.
fn pages(limits: Limits) -> u32 {
    u32::try_from(limits.instance_memory_max / (64 << 10)).expect("a ceiling within wasm32")
}

#[test]
fn doctor_takes_the_memory_ceiling_from_its_flag() {
    let (_directory, path) = write_module(
        "bigmem.wasm",
        &module_with(
            &[("isoplane", "http_fetch")],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
            Some(pages(Limits {
                instances_per_application: 16,
                module_bytes_compressed: 52428800,
                package_bytes: 52428800,
                instance_memory_max: 536870912,
                occupancy_ms_per_dispatch: 300000,
                subrequests_per_dispatch: 10000,
                pending_futures_per_dispatch: 4096,
            })),
        ),
    );
    let path = path.to_str().unwrap();

    let default = run(&["doctor", "--no-color", path]);
    assert_eq!(
        default.code,
        Some(2),
        "the default limits refuses memory only the larger memory ceiling admits: {}",
        default.stdout
    );
    let out = run(&[
        "doctor",
        "--no-color",
        path,
        "--guest-memory-bytes",
        "536870912",
    ]);
    assert_eq!(
        out.code,
        Some(0),
        "the selected limits admits memory the default limits refuses: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("numeric limits"),
        "the limits and where it came from are stated: {}",
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
fn doctor_reports_and_enforces_the_default_memory_ceiling() {
    let (_dir, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
            Some(8192),
        ),
    );
    let out = run(&["doctor", "--json", path.to_str().unwrap()]);
    assert_eq!(
        out.code,
        Some(2),
        "the default must reject a 512 MiB memory declaration"
    );
    let verdict: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();

    assert_eq!(
        verdict["guest_memory_bytes"], 134217728,
        "JSON must report the effective default memory ceiling"
    );
    assert_eq!(
        verdict["guest_memory_from"], "limits",
        "JSON must distinguish the numeric ceiling from an override"
    );
}

#[test]
fn doctor_admits_an_explicit_memory_override_without_changing_numeric_limits() {
    let (_dir, path) = write_module(
        "large.wasm",
        &module_with(
            &[],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
            Some(8192),
        ),
    );
    for format in ["--json", "--no-color"] {
        let out = run(&[
            "doctor",
            format,
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
                verdict["guest_memory_bytes"], 536870912_u64,
                "JSON must report the applied byte ceiling"
            );
            assert_eq!(
                verdict["guest_memory_from"], "--guest-memory-bytes",
                "JSON must identify the override source"
            );
        } else {
            assert!(
                out.stdout.contains("numeric limits")
                    && out
                        .stdout
                        .contains("guest memory ceiling: 536870912 bytes (--guest-memory-bytes)"),
                "text output must explain the independent memory ceiling: {}",
                out.stdout
            );
        }
    }
}

#[test]
fn a_doctor_memory_override_requires_a_declared_module_maximum() {
    let (_dir, path) = write_module(
        "unbounded.wasm",
        &module(
            &[],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
        ),
    );
    let out = run(&[
        "doctor",
        "--json",
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

#[test]
fn a_doctor_memory_override_does_not_admit_hostile_imports() {
    let (_dir, path) = write_module(
        "hostile.wasm",
        &module_with(
            &[("env", "pthread_create")],
            &[
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
                "memory",
            ],
            Some(8192),
        ),
    );
    let out = run(&[
        "doctor",
        "--json",
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
    let (_directory, path) = write_module(
        "valid.wasm",
        &module(
            &[],
            &[
                "memory",
                "isoplane_alloc",
                "isoplane_handle",
                "isoplane_wake",
            ],
        ),
    );
    for bytes in ["0", "1", "134217729", "4295032832"] {
        let out = run(&[
            "doctor",
            path.to_str().unwrap(),
            "--guest-memory-bytes",
            bytes,
        ]);
        assert_eq!(
            out.code,
            Some(1),
            "invalid memory policy must fail before reporting a module verdict"
        );
        assert!(
            out.stderr.contains("65536") && out.stderr.contains("4294967296"),
            "an invalid ceiling must name its required alignment and maximum: {}",
            out.stderr
        );
    }
}
