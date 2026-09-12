//! Test support: the one way a test obtains a guest module from `guests/`.
//!
//! The first access to each fixture runs its build script with `--if-stale`.
//! A fresh fixture validates its content receipt; stale or missing output is rebuilt.
//! Successful selections are reused for the remainder of the test process. The scripts
//! take a file lock, and this crate serialises callers within a process, so
//! concurrent test binaries never build the same fixture twice. A missing
//! toolchain fails the test with the script's message naming the tool.
//!
//! This crate is for `[dev-dependencies]` only; it panics on every failure
//! because its callers are tests.
#![forbid(unsafe_code)]

mod dotnet_template;

pub use dotnet_template::{aspnet_project, managed_cli};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BuildSelection {
    script: String,
    fixture: Option<String>,
    runtime: Option<String>,
}

static BUILT: Mutex<BTreeSet<BuildSelection>> = Mutex::new(BTreeSet::new());

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
    let mut built = BUILT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let selection = BuildSelection {
        script: script.to_owned(),
        fixture: fixture.map(str::to_owned),
        runtime: runtime.map(str::to_owned),
    };
    if built.contains(&selection) {
        return root;
    }
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
    built.insert(selection);
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
/// A name the script's manifest does not list fails here, naming the script,
/// rather than as a missing file or a stale module left in `guests/out`.
pub fn rust_path(name: &str) -> PathBuf {
    let root = build("guests/build.sh");
    let manifest = rust_manifest();
    assert!(
        manifest.iter().any(|built| built == name),
        "guests/build.sh does not build {name}.wasm; its GUESTS list names {}",
        manifest.join(", ")
    );
    root.join("guests/out").join(format!("{name}.wasm"))
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
    let path = dotnet_path(name, runtime);
    assert_attributed_to_feed(&path, runtime);
    read(&path)
}

fn dotnet_path(name: &str, runtime: GuestRuntime) -> PathBuf {
    build_selected(
        "guests/build-dotnet.sh",
        Some(name),
        Some(runtime.script_name()),
    )
    .join(runtime.output_directory())
    .join(format!("{name}.wasm"))
}

/// A module is only usable as evidence when the pack it links is the pack
/// the feed offers its runtime now; the publish records that identity
/// beside it.
fn assert_attributed_to_feed(module: &Path, runtime: GuestRuntime) {
    let linked = identity_beside(module);
    let offered = feed_runtime_identity(runtime);
    assert_eq!(
        linked,
        offered,
        "{} links a pack other than the one guest-runtime/out/feed offers {runtime}; the fixture build must republish it",
        module.display()
    );
}

/// The bytes of the runtime acceptance guest, `guest-runtime/tests/guest`,
/// published for `runtime` with the source-built pack, built if missing or
/// stale. A missing pack fails with the message naming
/// `guest-runtime/build.sh`.
pub fn runtime_acceptance_for(runtime: GuestRuntime) -> Vec<u8> {
    let path = runtime_acceptance_path(runtime);
    assert_attributed_to_feed(&path, runtime);
    read(&path)
}

fn runtime_acceptance_path(runtime: GuestRuntime) -> PathBuf {
    build_selected(
        "guest-runtime/tests/guest/build.sh",
        None,
        Some(runtime.script_name()),
    )
    .join("guest-runtime/tests/guest/out")
    .join(runtime.script_name())
    .join("guest_runtime_acceptance.wasm")
}

/// The identity of the pack a fixture was published with for `runtime`.
pub fn dotnet_runtime_identity(name: &str, runtime: GuestRuntime) -> String {
    identity_beside(&dotnet_path(name, runtime))
}

/// The local feed `guest-runtime/build.sh feed` publishes both packs to. An
/// installed package has no checkout to find it in, so a test points one at
/// this path through `WISP_GUEST_RUNTIME_FEED`, as a customer names theirs.
/// The feed is verified once per test process against current build inputs.
/// `WISP_TEST_RUNTIME_FEED` explicitly selects an independently verified artifact
/// feed instead, including when deliberately qualifying historical artifacts.
pub fn guest_runtime_feed() -> PathBuf {
    static FEED: OnceLock<PathBuf> = OnceLock::new();
    FEED.get_or_init(|| {
        let root = repo_root().join("guest-runtime");
        let explicit = std::env::var_os("WISP_TEST_RUNTIME_FEED");
        let feed = explicit
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("out/feed"));
        assert!(
            feed.is_absolute(),
            "WISP_TEST_RUNTIME_FEED must name an absolute artifact-feed path"
        );
        let mut command = Command::new("python3");
        command.arg(root.join("feed-identity.py"));
        if explicit.is_some() {
            command.arg("verify-artifact");
        } else {
            command.arg("verify-current").arg(&root);
        }
        let output = command
            .arg(&feed)
            .output()
            .expect("runtime artifact verifier must execute");
        assert!(
            output.status.success(),
            "runtime artifacts at {} are not valid for this test run:\n{}{}",
            feed.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if explicit.is_some() {
            eprintln!(
                "explicitly selected runtime artifacts: {}\n{}",
                feed.display(),
                String::from_utf8_lossy(&output.stdout)
            );
        }
        feed
    })
    .clone()
}

/// The identity of the pack the local feed currently offers `runtime`, from
/// the props file `guest-runtime/build.sh feed` writes: the pack version and
/// the digest NuGet records for the package, `version@sha512:digest`, the
/// same form a publish records beside its module.
pub fn feed_runtime_identity(runtime: GuestRuntime) -> String {
    let props = guest_runtime_feed().join("Wisp.GuestRuntime.props");
    let text = std::fs::read_to_string(&props).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); build the runtime packs with guest-runtime/build.sh",
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
    let prefix = match runtime {
        GuestRuntime::NativeAot => "WispGuestNativeAotPack",
        GuestRuntime::Mono => "WispGuestRuntimePack",
    };
    format!(
        "{}@sha512:{}",
        field(&format!("{prefix}Version")),
        field(&format!("{prefix}Sha512"))
    )
}

/// The ids of the packages the local feed offers, from the package file
/// names (`<id>.<version>.nupkg`, the version being the first dotted
/// segment that starts with a digit), for a consumer that maps package ids
/// to sources.
pub fn feed_package_ids() -> Vec<String> {
    let feed = guest_runtime_feed();
    let mut ids: Vec<String> = std::fs::read_dir(&feed)
        .unwrap_or_else(|e| panic!("{} is unreadable ({e})", feed.display()))
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let stem = name.strip_suffix(".nupkg")?.to_string();
            let version_at = stem
                .match_indices('.')
                .find(|(at, _)| stem[at + 1..].starts_with(|c: char| c.is_ascii_digit()))
                .map(|(at, _)| at)?;
            Some(stem[..version_at].to_string())
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn identity_beside(module: &Path) -> String {
    let sidecar = module.with_extension("wasm.runtime");
    let text = read(&sidecar);
    let identity = String::from_utf8_lossy(&text).trim().to_string();
    assert!(
        !identity.is_empty(),
        "{} records no runtime identity; a publish writes the pack it linked beside its module",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_fixture_selection_is_built_once_per_test_process() {
        let directory = tempfile::tempdir().expect("isolated fixture cache test directory");
        let script = directory.path().join("build.sh");
        std::fs::write(&script, "printf x >> \"${0}.calls\"\n")
            .expect("write controlled fixture builder");
        let script_name = script.to_str().expect("temporary script path is Unicode");
        build_selected(script_name, Some("first"), Some("mono"));
        build_selected(script_name, Some("first"), Some("mono"));
        assert_eq!(
            std::fs::read(script.with_extension("sh.calls")).expect("read build invocations"),
            b"x",
            "a fixture must not be rebuilt or revalidated during the same test process"
        );
        build_selected(script_name, Some("second"), Some("mono"));
        assert_eq!(
            std::fs::read(script.with_extension("sh.calls")).expect("read build invocations"),
            b"xx",
            "a different fixture selection must establish its own startup artifacts"
        );
    }

    #[test]
    fn a_failed_fixture_build_is_not_cached_as_successful() {
        let directory = tempfile::tempdir().expect("isolated failed build test directory");
        let script = directory.path().join("build.sh");
        std::fs::write(&script, "exit 9\n").expect("write failing fixture builder");
        let script_name = script.to_str().expect("temporary script path is Unicode");
        let failure = std::panic::catch_unwind(|| build_selected(script_name, None, None));
        assert!(
            failure.is_err(),
            "a failed build must fail the requesting test"
        );
        std::fs::write(&script, "printf x >> \"${0}.calls\"\n")
            .expect("repair the controlled fixture builder");
        build_selected(script_name, None, None);
        assert_eq!(
            std::fs::read(script.with_extension("sh.calls")).expect("read successful retry"),
            b"x",
            "a failed attempt must not let a later caller consume missing artifacts"
        );
    }
}
