#![forbid(unsafe_code)]
fn main() -> std::io::Result<()> {
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../guest-sdks/dotnet/native/isoplane_native_io.h"
        ),
        isoplane_abi::native_io::canonical::native_wrappers(),
    )
}
