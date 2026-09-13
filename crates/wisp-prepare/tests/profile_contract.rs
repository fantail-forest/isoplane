use wisp_prepare::{ManagedComponents, ManagedMode, RuntimeProfile};

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
