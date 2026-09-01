//! A stock .NET module declares a large `wasi:*` surface that the raw
//! capability table would reject, and only a couple of those imports are ever
//! reached — among them `wasi:random` (a second entropy path) and
//! `wasi:clocks/monotonic-clock` (the fine timer the platform denies). The
//! delegating adapter resolves them under the no-new-channels rule.
//!
//! That surface came off a real NativeAOT-LLVM artifact, so the check has to
//! run against one too. A synthetic `.wat` fixture with four `wasi:*` imports
//! proves the linker works; it cannot prove the *whitelist is the right size*,
//! because the fixture's surface is whatever the fixture author typed. This
//! test loads the module the toolchain actually emits and asserts two things
//! about it:
//!
//! 1. it **loads**, which it could not before the adapter reached the data
//!    plane's linker — validation and pre-initialization already passed, so
//!    the failure used to surface at activation, which is the worst place for
//!    a capability decision to appear;
//! 2. the adapter drew on **exactly two** `wisp:platform` capabilities. That
//!    is the no-new-channels rule as an assertion: dozens of declarations came
//!    in and two capabilities went out, both already in the tenant-facing
//!    manifest.
//!
//! ## Why this fails rather than skips
//!
//! `guests/dotnet/out/wisp_guest_dotnet_stock.wasm` is **built by this test**,
//! via `guests/build-dotnet.sh --if-stale`. It used to be committed, on the
//! argument that building it needs a .NET SDK most checkouts do not have — but
//! that describes checkouts which cannot run this test either: the Dockerfile
//! installs the SDK and `scripts/dev` is the enforced environment for every
//! build and test in this repo. What the old rule actually bought was six
//! tracked binaries that went stale the moment anyone edited the shared PAL,
//! with tests still asserting against them.
//!
//! The rule that survives is the one that mattered: this test **fails** rather
//! than skips. A skip buys nothing and costs the thing that matters — a green
//! test that asserted nothing, with no aggregator to notice, because
//! `cargo test` has no notion of "ran but proved nothing". Building the fixture
//! removes the excuse for skipping instead of weakening the assertion.
//!
//! This test once skipped, on the mistaken belief that the artifact was
//! untracked build output. It was untracked, briefly, in a working tree where
//! another track had built it before committing it — which is a decent
//! illustration that "is this fixture tracked" is a question about the
//! repository and not about whatever happens to be in one's own `git status`
//! at the time.

use wisp_engine::{EngineConfig, WispEngine};

fn stock_module() -> (String, Vec<u8>) {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/wisp-engine has a grandparent")
        .to_path_buf();
    // Built on demand rather than committed — see this test's header. The
    // script is a no-op when the fixture is fresh and takes an flock, because
    // cargo runs test binaries concurrently and two of them call it.
    let script = root.join("guests/build-dotnet.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .arg("--if-stale")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "{} failed, so the stock .NET fixture could not be built:\n{}{}",
        script.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let p = root.join("guests/dotnet/out/wisp_guest_dotnet_stock.wasm");
    let bytes = std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "{} is still missing after a successful build: {e}\n\nThat is a bug in \
             guests/build-dotnet.sh rather than in your checkout — skipping here would turn \
             the only engine-level assertion about the WASI surface into a green test that \
             proved nothing.",
            p.display()
        )
    });
    (p.display().to_string(), bytes)
}

#[test]
fn the_stock_dotnet_module_loads_through_the_pal_adapter() {
    let (path, wasm) = stock_module();

    let e = WispEngine::new(EngineConfig {
        max_instances: 8,
        // A real .NET guest is not a 4-page `.wat` fixture.
        max_memory_bytes: 256 << 20,
        guard_size_bytes: 64 << 20,
        ..EngineConfig::default()
    })
    .expect("engine");

    let t = e.load_template(&wasm).unwrap_or_else(|err| {
        panic!(
            "the stock .NET module at {path} did not load: {err:#}\n\nThe module declares a \
             large `wasi:*` surface, which the platform resolves by composing the PAL adapter \
             at deploy. If this fails on an undeclared import, the whitelist is the wrong size \
             and that is a design conversation, not a linker bug."
        )
    });

    assert!(t.uses_pal(), "a stock .NET module must need the adapter");

    let composed = t.pal_composed();
    let delegated: Vec<&str> =
        composed.iter().filter_map(|d| d.disposition.capability()).collect();
    let trapped = composed.iter().filter(|d| !d.disposition.is_delegated()).count();

    eprintln!(
        "{path}\n  {} wasi: imports composed — {} trap-on-call, delegated to {:?}",
        composed.len(),
        trapped,
        delegated
    );

    // Not an equality: the surface is a closure over what tenant code drags
    // out of the BCL, not a fixed list. But a stock module's surface is dozens
    // of imports, and anything much smaller means the adapter did not run.
    assert!(
        composed.len() >= 30,
        "only {} wasi: imports composed; a stock .NET module declares far more",
        composed.len()
    );

    // **The no-new-channels rule, as an assertion.** Thirty-odd declarations
    // came in; exactly two `wisp:platform` capabilities went out, and both are
    // already in the tenant-facing manifest. If a third ever appears here, a
    // capability has entered through a WASI door that the manifest does not
    // describe — which is the failure this whole table exists to prevent, and
    // it would be invisible from every other test.
    let mut unique: Vec<&str> = delegated.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique,
        vec!["clock_now_coarse", "entropy_fill"],
        "the adapter drew on capabilities beyond the declared two: {unique:?}"
    );
    assert!(trapped > 0, "most of a stock module's wasi: surface is toolchain residue and must trap");
}
