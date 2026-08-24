//! The `wisp` binary, driven as a user would drive it.
//!
//! `wisp doctor` gets the most attention here because it is the tenant-facing
//! form of invariant 4 — "what can this code do?" is a static question — and a
//! claim like that is only worth what its user-visible answer is worth.

use std::process::Command;

const WISP: &str = env!("CARGO_BIN_EXE_wisp");

/// A minimal core-wasm module: header, one type, the given imports, a memory
/// and the given exports. Hand-assembled so the test has no build-time
/// dependency on a wasm toolchain.
fn module(imports: &[(&str, &str)], exports: &[&str]) -> Vec<u8> {
    let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    m.extend_from_slice(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00]); // type: () -> ()

    let mut body = vec![imports.len() as u8];
    for (module_, field) in imports {
        body.push(module_.len() as u8);
        body.extend_from_slice(module_.as_bytes());
        body.push(field.len() as u8);
        body.extend_from_slice(field.as_bytes());
        body.extend_from_slice(&[0x00, 0x00]);
    }
    m.push(0x02);
    m.push(body.len() as u8);
    m.extend_from_slice(&body);

    m.extend_from_slice(&[0x05, 0x03, 0x01, 0x00, 0x01]); // memory: 1 page

    let mut body = vec![exports.len() as u8];
    for e in exports {
        body.push(e.len() as u8);
        body.extend_from_slice(e.as_bytes());
        if *e == "memory" {
            body.extend_from_slice(&[0x02, 0x00]);
        } else {
            body.extend_from_slice(&[0x00, 0x00]);
        }
    }
    m.push(0x07);
    m.push(body.len() as u8);
    m.extend_from_slice(&body);
    m
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
    let o = Command::new(WISP).args(args).output().expect("running the wisp binary");
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
    for name in ["http_fetch", "http_body_read", "init_hash_seed", "clock_now_coarse"] {
        assert!(out.stdout.contains(name), "missing {name} from the manifest");
    }
    assert!(
        out.stdout.contains("http_body_read") && out.stdout.contains("future"),
        "future-returning imports remain visible"
    );
    assert!(out.stdout.contains("sensitive"));
    // The plain-English answer, which is the actual point of the command.
    assert!(out.stdout.contains("SO: THIS CODE CAN"));
    assert!(out.stdout.contains("make outbound HTTP requests"));
    assert!(out.stdout.contains("no threads, no files, no ambient sockets"));
    assert!(out.stdout.contains("✓ every import is a declared capability"));
}

#[test]
fn doctor_rejects_a_module_that_reaches_for_an_undeclared_capability() {
    // V1 (§4.3). The two undeclared imports here are the two things a real
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

#[test]
fn doctor_notices_a_module_that_cannot_be_activated_at_all() {
    let p = write_module("noexports.wasm", &module(&[("wisp", "http_fetch")], &["memory"]));
    let out = run(&["--no-color", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(2));
    assert!(out.stdout.contains("required export `wisp_handle` is missing"), "{}", out.stdout);
}

#[test]
fn doctor_has_a_machine_readable_form() {
    let p = write_module(
        "json.wasm",
        &module(&[("wisp", "http_fetch")], &["wisp_alloc", "wisp_handle", "wisp_wake", "memory"]),
    );
    let out = run(&["--json", "doctor", p.to_str().unwrap()]);
    assert_eq!(out.code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(v["rejected"], false);
    assert_eq!(v["capabilities"][0]["name"], "http_fetch");
    assert_eq!(v["capabilities"][0]["group"], "http");
    assert_eq!(v["capabilities"][0]["kind"], "future");
    assert_eq!(v["capabilities"][0]["wit"], "http.fetch");
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
