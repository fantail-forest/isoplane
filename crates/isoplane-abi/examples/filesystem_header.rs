#![forbid(unsafe_code)]
fn main() -> std::io::Result<()> {
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../guest-sdks/dotnet/native/isoplane_filesystem.h"
        ),
        isoplane_abi::filesystem::native_wrappers(),
    )
}
