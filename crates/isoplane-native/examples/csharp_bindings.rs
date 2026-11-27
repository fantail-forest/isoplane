//! Writes the C# declarations of the native library's entry points and the
//! records they exchange to `tools/Isoplane.Native/NativeMethods.g.cs`.
#![forbid(unsafe_code)]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = |file: &str| format!("{}/src/{file}", env!("CARGO_MANIFEST_DIR"));
    csbindgen::Builder::default()
        .input_extern_file(source("lib.rs"))
        .input_extern_file(source("interop.rs"))
        .input_extern_file(source("session.rs"))
        .input_extern_file(source("session_events.rs"))
        .input_extern_file(source("process_records.rs"))
        .input_extern_file(source("runtime_cache.rs"))
        .input_extern_file(source("local/runtime.rs"))
        .csharp_namespace("Isoplane.Native.Interop")
        .csharp_dll_name("isoplane_native")
        .csharp_generate_const_filter(|name| name == "ABI_VERSION")
        .generate_csharp_file(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/Isoplane.Native/NativeMethods.g.cs"
        ))
}
