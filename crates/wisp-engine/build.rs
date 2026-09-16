#![forbid(unsafe_code)]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(manifest)?;
    let value: toml::Value = toml::from_str(&text)?;
    let revision = value["workspace"]["dependencies"]["wasmtime"]["rev"]
        .as_str()
        .ok_or("Wasmtime must be pinned to a fork commit")?;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Wasmtime requires a full commit hash".into());
    }
    println!("cargo:rustc-env=WISP_WASMTIME_REVISION={revision}");
    Ok(())
}
