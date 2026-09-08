//! A stock .NET module declares a large `wasi:*` surface and reaches a
//! handful of it. A synthetic fixture proves the linker composes; only the
//! module the toolchain emits can prove the whitelist is the right size and
//! that exactly two `wisp:platform` capabilities come out of it.

use wisp_engine::{EngineConfig, WispEngine};

#[test]
fn the_stock_dotnet_module_loads_through_the_pal_adapter() {
    let wasm = wisp_fixtures::dotnet("wisp_guest_dotnet_stock");

    let e = WispEngine::new(EngineConfig {
        max_instances: 8,
        max_memory_bytes: 256 << 20,
        guard_size_bytes: 64 << 20,
        ..EngineConfig::default()
    })
    .expect("engine");

    let t = e.load_template(&wasm).unwrap_or_else(|err| {
        panic!(
            "the stock .NET module did not load: {err:#}; if this is an undeclared import, \
             the PAL whitelist is the wrong size"
        )
    });

    assert!(t.uses_pal(), "a stock .NET module must need the adapter");

    let composed = t.pal_composed();
    let mut delegated: Vec<&str> = composed
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    let trapped = composed
        .iter()
        .filter(|d| !d.disposition.is_delegated())
        .count();

    // Not an equality: the surface is a closure over what tenant code drags
    // out of the BCL. A stock module declares dozens; far fewer means the
    // adapter did not run.
    assert!(
        composed.len() >= 30,
        "only {} wasi: imports composed; a stock .NET module declares far more",
        composed.len()
    );

    delegated.sort_unstable();
    delegated.dedup();
    assert_eq!(
        delegated,
        vec![
            "clock_now_coarse",
            "entropy_fill",
            "filesystem_call",
            "poll_call",
            "socket_call"
        ],
        "the adapter drew on a capability beyond the declared set, so a capability has \
         entered through a WASI door the manifest does not describe"
    );
    assert!(
        trapped > 0,
        "most of a stock module's wasi: surface is toolchain residue and must trap"
    );
}
