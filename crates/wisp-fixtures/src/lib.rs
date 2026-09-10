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

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/wisp-fixtures has a grandparent")
        .to_path_buf()
}

fn build(script: &str) -> PathBuf {
    build_selected(script, None, None)
}

fn build_selected(script: &str, fixture: Option<&str>, runtime: Option<&str>) -> PathBuf {
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
    if let Some(runtime) = runtime {
        command.arg("--runtime").arg(runtime);
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

/// The execution runtime a C# fixture is published for. Every behaviour a
/// customer sees is expected in both; a test that drives one fixture through
/// the engine names the runtime so its failures do too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuestRuntime {
    /// NativeAOT-LLVM: the guest and the reachable BCL compiled to one module.
    NativeAot,
    /// The Mono interpreter with the guest's IL bundled into the module.
    Mono,
}

impl GuestRuntime {
    /// Both runtimes, for tests that prove one behaviour in each.
    pub const ALL: [GuestRuntime; 2] = [GuestRuntime::NativeAot, GuestRuntime::Mono];

    fn script_name(self) -> &'static str {
        match self {
            GuestRuntime::NativeAot => "nativeaot",
            GuestRuntime::Mono => "mono",
        }
    }

    fn output_directory(self) -> &'static str {
        match self {
            GuestRuntime::NativeAot => "guests/dotnet/out",
            GuestRuntime::Mono => "guests/dotnet/out/mono",
        }
    }
}

impl std::fmt::Display for GuestRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.script_name())
    }
}

/// The bytes of a NativeAOT C# guest `guests/build-dotnet.sh` writes under
/// `guests/dotnet/out`, built if missing or stale.
pub fn dotnet(name: &str) -> Vec<u8> {
    dotnet_for(name, GuestRuntime::NativeAot)
}

/// The bytes of a C# guest published for `runtime`, built if missing or
/// stale. Only the fixtures `guests/build-dotnet.sh` lists for Mono exist in
/// that runtime; asking for another fails with the script's message.
pub fn dotnet_for(name: &str, runtime: GuestRuntime) -> Vec<u8> {
    let path = build_selected(
        "guests/build-dotnet.sh",
        Some(name),
        Some(runtime.script_name()),
    )
    .join(runtime.output_directory())
    .join(format!("{name}.wasm"));
    if runtime == GuestRuntime::Mono {
        assert_attributed_to_feed(&path);
    }
    read(&path)
}

/// A Mono module is only usable as evidence when the pack it links is the
/// pack the feed offers now; the publish records that identity beside it.
fn assert_attributed_to_feed(module: &Path) {
    let linked = identity_beside(module);
    let offered = feed_runtime_identity();
    assert_eq!(
        linked,
        offered,
        "{} links a runtime pack other than the one guest-runtime/out/feed offers; the fixture build must republish it",
        module.display()
    );
}

/// The bytes of the runtime acceptance guest, `guest-runtime/tests/guest`,
/// published with the source-built runtime pack, built if missing or stale.
/// A missing pack fails with the message naming `guest-runtime/build.sh`.
pub fn runtime_acceptance() -> Vec<u8> {
    let path = runtime_acceptance_path();
    assert_attributed_to_feed(&path);
    read(&path)
}

fn runtime_acceptance_path() -> PathBuf {
    build("guest-runtime/tests/guest/build.sh")
        .join("guest-runtime/tests/guest/out/guest_runtime_acceptance.wasm")
}

/// The identity of the runtime pack a Mono fixture was published with; a
/// NativeAOT fixture has none, its runtime being the pinned compiler package.
pub fn dotnet_runtime_identity(name: &str, runtime: GuestRuntime) -> Option<String> {
    match runtime {
        GuestRuntime::NativeAot => None,
        GuestRuntime::Mono => Some(identity_beside(
            &build_selected(
                "guests/build-dotnet.sh",
                Some(name),
                Some(runtime.script_name()),
            )
            .join(runtime.output_directory())
            .join(format!("{name}.wasm")),
        )),
    }
}

/// The identity of the runtime pack the local feed currently offers, from
/// the props file `guest-runtime/build.sh feed` writes: the pack version and
/// the digest NuGet records for the package, `version@sha512:digest`, the
/// same form a Mono publish records beside its module.
pub fn feed_runtime_identity() -> String {
    let props = repo_root().join("guest-runtime/out/feed/Wisp.GuestRuntime.props");
    let text = std::fs::read_to_string(&props).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); build the runtime pack with guest-runtime/build.sh",
            props.display()
        )
    });
    let field = |name: &str| {
        text.split(&format!("<{name}>"))
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| panic!("{} names no {name}", props.display()))
            .to_string()
    };
    format!(
        "{}@sha512:{}",
        field("WispGuestRuntimePackVersion"),
        field("WispGuestRuntimePackSha512")
    )
}

fn identity_beside(module: &Path) -> String {
    let sidecar = module.with_extension("wasm.runtime");
    let text = read(&sidecar);
    let identity = String::from_utf8_lossy(&text).trim().to_string();
    assert!(
        !identity.is_empty(),
        "{} records no runtime identity; a Mono publish writes the pack it linked beside its module",
        sidecar.display()
    );
    identity
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
