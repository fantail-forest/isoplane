//! Readers for fixtures prepared by Task before the test suite starts.
//! This crate is for test dependencies and panics when required artifacts are absent.

mod compiled_runtime;
mod dotnet_projects;

pub use compiled_runtime::{
    compiled_runtime_artifacts, compiled_runtime_cache, engine_config, instrumented_runtime,
    load_compiled_runtime,
};

pub use dotnet_projects::{
    gated_project, handler_project, managed_cli, release_version, template_project,
    testing_consumer,
};

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/isoplane-fixtures has a grandparent")
        .to_path_buf()
}

/// The debugger test interpreter prepared by Task.
pub fn debugger_python() -> PathBuf {
    std::env::var_os("ISOPLANE_TEST_DEBUGGER_PYTHON")
        .map(PathBuf::from)
        .expect("run tests through Task to select the prepared debugger Python environment")
}

/// Cargo output selected by Task through `CARGO_TARGET_DIR`.
pub fn target_directory() -> &'static Path {
    static TARGET: OnceLock<PathBuf> = OnceLock::new();
    TARGET.get_or_init(|| {
        let path = std::env::var_os("CARGO_TARGET_DIR")
            .unwrap_or_else(|| panic!("CARGO_TARGET_DIR is unset; run tests through task test"));
        repo_root().join(path)
    })
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "cannot read prepared fixture {} ({e}); run task fixtures before cargo test",
            path.display()
        )
    })
}

/// The path of a module `guests/build.sh` writes, prepared by Task.
/// A name the script's manifest does not list fails here, naming the script,
/// rather than as a missing file or a stale module left in `.build/wasm32/rust-guests`.
pub fn rust_path(name: &str) -> PathBuf {
    let root = repo_root();
    let manifest = rust_manifest();
    assert!(
        manifest.iter().any(|built| built == name),
        "guests/build.sh does not build {name}.wasm; its GUESTS list names {}",
        manifest.join(", ")
    );
    root.join(".build/wasm32/rust-guests")
        .join(format!("{name}.wasm"))
}

/// The bytes of a module `guests/build.sh` writes, prepared by Task.
pub fn rust(name: &str) -> Vec<u8> {
    read(&rust_path(name))
}

/// The names of every module `guests/build.sh` writes, from the manifest the
/// script leaves beside them.
pub fn rust_manifest() -> Vec<String> {
    let manifest = repo_root().join(".build/wasm32/rust-guests/manifest");
    String::from_utf8_lossy(&read(&manifest))
        .lines()
        .map(str::to_string)
        .collect()
}

/// The runtime used by C# sandbox fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuestRuntime {
    /// The shared Mono interpreter with application IL supplied separately.
    Mono,
}

impl GuestRuntime {
    /// The retained runtime exercised by the fixture harness.
    pub const ALL: [GuestRuntime; 1] = [GuestRuntime::Mono];

    fn script_name(self) -> &'static str {
        match self {
            GuestRuntime::Mono => "mono",
        }
    }
}

impl std::fmt::Display for GuestRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.script_name())
    }
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
        "{} links a pack other than the one .build/wasm32/runtime/feed offers {runtime}; the fixture build must republish it",
        module.display()
    );
}

/// The application-independent Mono runtime, `guest-sdks/dotnet/MonoRuntime`,
/// published with the source-built pack and prepared by Task. It
/// runs whichever package the host serves it.
pub fn mono_runtime() -> Vec<u8> {
    read(&mono_runtime_module())
}

/// The path of the shared Mono runtime module, as the SDK package carries it
/// beside its Mono pack identity, prepared by Task.
pub fn mono_runtime_module() -> PathBuf {
    shared_runtime("Isoplane.MonoRuntime.wasm")
}

/// The shared runtime's local-debug variant, which connects its debugger
/// agent to the session the host serves it.
pub fn mono_debug_runtime() -> Vec<u8> {
    read(&mono_debug_runtime_module())
}

/// The prepared local-debug runtime module shared by isolated debugger sessions.
pub fn mono_debug_runtime_module() -> PathBuf {
    shared_runtime("Isoplane.MonoRuntime.Debug.wasm")
}

fn shared_runtime(name: &str) -> PathBuf {
    let directory = std::env::var_os("ISOPLANE_TEST_RUNTIME_MODULES")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".build/wasm32/runtime-modules"));
    let path = directory.join(name);
    assert_attributed_to_feed(&path, GuestRuntime::Mono);
    path
}

/// The path of the package fixture `guest-runtime/tests/packages/<name>`,
/// the archive `isoplane bundle` writes, prepared by Task.
pub fn mono_package(name: &str) -> PathBuf {
    repo_root()
        .join(".build/clr-anycpu/package-fixtures")
        .join(format!("{name}.tar"))
}

/// Host-specific compiler baseline and deltas shared by Hot Reload scenarios.
pub fn hot_reload_artifacts() -> PathBuf {
    std::env::var_os("ISOPLANE_TEST_HOT_RELOAD_ARTIFACTS")
        .map(PathBuf::from)
        .expect("run task test:debugger to select the prepared Hot Reload fixture")
}

/// The runtime package feed prepared or explicitly selected by Task.
pub fn guest_runtime_feed() -> PathBuf {
    std::env::var_os("ISOPLANE_TEST_RUNTIME_FEED")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".build/wasm32/runtime/feed"))
}

/// The runtime cache Task filled by running the packaged CLI's
/// `prepare-runtime`, which sessions of the installed CLI reuse instead of
/// compiling the shared runtime themselves.
pub fn runtime_cache() -> PathBuf {
    let path = std::env::var_os("ISOPLANE_TEST_RUNTIME_CACHE")
        .map(PathBuf::from)
        .expect("run task fixtures:runtime to select the prepared runtime cache");
    assert!(
        path.is_dir(),
        "the prepared runtime cache is missing at {}; run task fixtures:runtime",
        path.display()
    );
    path
}

/// The identity of the pack the local feed currently offers `runtime`, from
/// the props file `guest-runtime/build.sh feed` writes: the pack version and
/// the digest NuGet records for the package, `version@sha512:digest`, the
/// same form a publish records beside its module.
pub fn feed_runtime_identity(runtime: GuestRuntime) -> String {
    let props = guest_runtime_feed().join("Isoplane.GuestRuntime.props");
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
        GuestRuntime::Mono => "IsoplaneGuestRuntimePack",
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
    package_ids(&guest_runtime_feed())
}

fn package_ids(feed: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(feed)
        .unwrap_or_else(|e| panic!("{} is unreadable ({e})", feed.display()))
        .map(|entry| entry.expect("read a package feed entry"))
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

/// An immutable application file served to a shared-runtime probe.
pub struct ProbeFile {
    /// Absolute guest-visible path.
    pub path: String,
    /// Managed assembly, resource, or application manifest contents.
    pub bytes: Vec<u8>,
}

/// Reads the guest-visible files from a prepared managed probe package.
pub fn managed_probe(name: &str) -> Vec<ProbeFile> {
    let path = repo_root()
        .join(".build/clr-anycpu/probes")
        .join(format!("{name}.tar"));
    let bytes = read(&path);
    let mut archive = tar::Archive::new(bytes.as_slice());
    archive
        .entries()
        .expect("read managed probe archive")
        .filter_map(|entry| {
            let mut entry = entry.expect("read managed probe entry");
            let path = entry.path().expect("read managed probe path").into_owned();
            if path == Path::new("runtime.json") {
                return None;
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read managed probe file");
            Some(ProbeFile {
                path: format!(
                    "{}/{}",
                    isoplane_abi::filesystem::APPLICATION_PACKAGE_ROOT,
                    path.display()
                ),
                bytes,
            })
        })
        .collect()
}
