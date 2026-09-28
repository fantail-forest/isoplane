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
