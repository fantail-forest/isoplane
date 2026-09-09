//! Test support: the one way a test obtains a guest module from `guests/`.
//!
//! Each accessor runs the owning build script with `--if-stale`, so a fresh
//! fixture costs a stat and a stale or missing one is rebuilt. The scripts
//! take a file lock, and this crate serialises callers within a process, so
//! concurrent test binaries never build the same fixture twice. A missing
//! toolchain fails the test with the script's message naming the tool.
//!
//! This crate is for `[dev-dependencies]` only; it panics on every failure
//! because its callers are tests.
#![forbid(unsafe_code)]

mod dotnet_template;

pub use dotnet_template::{aspnet_project, managed_cli};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static BUILD: Mutex<()> = Mutex::new(());

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/wisp-fixtures has a grandparent")
        .to_path_buf()
}

fn build(script: &str) -> PathBuf {
    build_selected(script, None)
}

fn build_selected(script: &str, fixture: Option<&str>) -> PathBuf {
    let root = repo_root();
    let path = root.join(script);
    let _one_at_a_time = BUILD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut command = Command::new("bash");
    command.arg(&path).arg("--if-stale");
    if let Some(name) = fixture {
        command.arg("--fixture").arg(name);
    }
    let out = command
        .env("WISP_BUILD_QUIET", "1")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", path.display()));
    assert!(
        out.status.success(),
        "{} failed, so the fixture could not be built:\n{}{}",
        path.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    root
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "{} is missing after a successful build ({e}); the build script no longer writes it",
            path.display()
        )
    })
}

/// The path of a module `guests/build.sh` writes, built if missing or stale.
pub fn rust_path(name: &str) -> PathBuf {
    build("guests/build.sh")
        .join("guests/out")
        .join(format!("{name}.wasm"))
}

/// The bytes of a module `guests/build.sh` writes, built if missing or stale.
pub fn rust(name: &str) -> Vec<u8> {
    read(&rust_path(name))
}

/// The names of every module `guests/build.sh` writes, from the manifest the
/// script leaves beside them.
pub fn rust_manifest() -> Vec<String> {
    let manifest = build("guests/build.sh").join("guests/out/manifest");
    String::from_utf8_lossy(&read(&manifest))
        .lines()
        .map(str::to_string)
        .collect()
}

/// The bytes of a C# guest `guests/build-dotnet.sh` writes under
/// `guests/dotnet/out`, built if missing or stale.
pub fn dotnet(name: &str) -> Vec<u8> {
    let path = build_selected("guests/build-dotnet.sh", Some(name))
        .join("guests/dotnet/out")
        .join(format!("{name}.wasm"));
    read(&path)
}

/// Stages an isolated C# source fixture for tests of publishing and source reload.
/// The command under test builds it on demand; no shared build outputs are used.
pub fn dotnet_project(destination: &Path) -> PathBuf {
    let root = repo_root();
    copy_sources(&root.join("guest-sdks/dotnet"), &destination.join("sdk"));
    copy_sources(&root.join("guests/publish-cs"), &destination.join("app"));
    std::fs::copy(root.join("nuget.config"), destination.join("nuget.config"))
        .expect("copy the compiler feed configuration");
    destination.join("app")
}

fn copy_sources(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create isolated C# source directory");
    for entry in std::fs::read_dir(source).expect("read C# fixture sources") {
        let entry = entry.expect("read C# source entry");
        if matches!(entry.file_name().to_str(), Some("bin" | "obj")) {
            continue;
        }
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("inspect C# source").is_dir() {
            copy_sources(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy C# fixture source");
        }
    }
}

/// Prove that a native CPU clock measures work rather than returning a dummy value.
/// Panics if either read fails or CPU work does not advance the clock.
pub fn assert_cpu_clock_advances(read_clock: impl Fn() -> Option<u64>) {
    let before = read_clock().expect("the native CPU clock is available");
    let mut value = 0u64;
    for i in 0..3_000_000u64 {
        value = std::hint::black_box(value.wrapping_add(i ^ value));
    }
    let after = read_clock().expect("the native CPU clock remains available");
    assert!(
        after > before,
        "CPU work must advance the native clock, including on ARM"
    );
}
