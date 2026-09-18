use isoplane_prepare::{ManagedComponents, ManagedMode, RuntimeProfile};

#[test]
fn managed_publisher_profiles_follow_the_native_profile_contract() -> anyhow::Result<()> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tools/BuildProtocol/runtime-profiles.json"
    ))?;
    for (name, mode) in [
        ("Interpreted", ManagedMode::Mono),
        ("NativeAot", ManagedMode::NativeAot),
    ] {
        let profile = RuntimeProfile::DotNet10 {
            mode,
            components: Box::new(ManagedComponents {
                sdk: [1; 32],
                runtime: [2; 32],
                libraries: [3; 32],
                compiler: [4; 32],
                platform: [5; 32],
                wasi: [6; 32],
                native_layout: [7; 32],
            }),
        };
        assert_eq!(
            serde_json::to_value(&profile)?,
            fixture[name],
            "the managed publisher fixture must match native profile serialization"
        );
        assert_eq!(
            serde_json::from_value::<RuntimeProfile>(fixture[name].clone())?,
            profile,
            "native preparation must consume the managed publisher profile"
        );
    }
    Ok(())
}

#[test]
fn managed_memory_budgets_follow_native_admission_limits() -> anyhow::Result<()> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tools/BuildProtocol/guest-memory-budgets.json"
    ))?;
    assert_eq!(
        fixture["PageBytes"],
        isoplane_abi::WASM_PAGE_SIZE as u64,
        "managed budgets must use the native page size"
    );
    assert_eq!(
        fixture["MaximumBytes"],
        isoplane_abi::config::WASM32_MEMORY_MAX_BYTES,
        "managed budgets must respect wasm32 addressing"
    );
    assert_eq!(
        fixture["DefaultBytes"],
        isoplane_abi::config::Limits::default()
            .limits()
            .instance_memory_max,
        "managed defaults must retain the standard memory budget"
    );
    Ok(())
}
