//! **The finding, closed** (spec.md §4.3; Appendix C **F7**).
//!
//! > *A stock .NET module declares 32 `wasi:*` imports the validator rejects;
//! > recording stubs show 2–3 reached, including `wasi:random` (a second
//! > entropy path) and `wasi:clocks/monotonic-clock` (the fine timer §8.4
//! > denies). Resolved by the delegating adapter under the no-new-channels
//! > rule.*
//!
//! F7 was measured against a real NativeAOT-LLVM artifact, so closing it has
//! to be measured against one too. A synthetic `.wat` fixture with four
//! `wasi:*` imports proves the linker works; it cannot prove the *whitelist
//! is the right size*, because the fixture's surface is whatever the fixture
//! author typed. This test loads the module the toolchain actually emits and
//! asserts two things about it:
//!
//! 1. it **loads**, which it could not before the adapter reached the data
//!    plane's linker — validation and pre-initialization already passed, so
//!    the failure used to surface at activation, which is the worst place for
//!    a capability decision to appear;
//! 2. the adapter drew on **exactly two** `wisp:platform` capabilities. That
//!    number is the no-new-channels rule as an assertion: 32 declarations came
//!    in and two capabilities went out, both already in the tenant-facing
//!    manifest.
//!
//! The artifact is not tracked in git (it is ~900 KB of build output), so an
//! absent one is reported and skipped rather than failed. Its build command is
//! in `guests/dotnet/build.sh`.

use wisp_engine::{EngineConfig, WispEngine};

fn stock_module() -> Option<(String, Vec<u8>)> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())?
        .to_path_buf();
    let p = root.join("guests/dotnet/out/wisp_guest_dotnet_stock.wasm");
    let bytes = std::fs::read(&p).ok()?;
    Some((p.display().to_string(), bytes))
}

#[test]
fn the_stock_dotnet_module_loads_through_the_pal_adapter() {
    let Some((path, wasm)) = stock_module() else {
        eprintln!(
            "SKIP: no stock .NET module built. This is F7's subject, so the test means nothing \
             without it. Build it with:\n    WISP_OUT=wisp_guest_dotnet_stock \
             guests/dotnet/build.sh -p:WispPalObject="
        );
        return;
    };

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
            "the stock .NET module at {path} did not load: {err:#}\n\nThis is Appendix C F7: \
             the module declares ~32 `wasi:*` imports, and §4.3 resolves them by composing the \
             PAL adapter at deploy. If this fails on an undeclared import, the whitelist is the \
             wrong size and that is a design conversation, not a linker bug."
        )
    });

    assert!(t.uses_pal(), "a stock .NET module must need the adapter");

    let composed = t.pal_composed();
    let delegated: Vec<&str> =
        composed.iter().filter_map(|d| d.disposition.capability()).collect();
    let trapped = composed.iter().filter(|d| !d.disposition.is_delegated()).count();

    eprintln!(
        "F7 CLOSED: {path}\n  {} wasi: imports composed — {} trap-on-call, delegated to {:?}",
        composed.len(),
        trapped,
        delegated
    );

    // F7's number. Not asserted as an equality on 32 — the surface is a
    // closure over what tenant code drags out of the BCL, not a fixed list
    // (docs/dotnet-findings.md §1.4) — but it must be the shape F7 described.
    assert!(
        composed.len() >= 30,
        "only {} wasi: imports composed; F7 measured 32 on the stock module",
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
