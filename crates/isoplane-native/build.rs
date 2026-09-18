fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-env-changed=ISOPLANE_GENERATE_BINDINGS");
    let output = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("NativeMethods.g.cs");
    csbindgen::Builder::default()
        .input_extern_file("src/lib.rs")
        .csharp_namespace("Isoplane.Native.Interop")
        .csharp_dll_name("isoplane_native")
        .csharp_generate_const_filter(|name| name == "ABI_VERSION")
        .generate_csharp_file(&output)?;
    if std::env::var_os("ISOPLANE_GENERATE_BINDINGS").is_some() {
        std::fs::copy(output, "../../tools/Isoplane.Native/NativeMethods.g.cs")?;
    }
    Ok(())
}
