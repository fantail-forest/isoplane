#![forbid(unsafe_code)]
fn main() -> std::io::Result<()> {
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../guest-sdks/dotnet/native/isoplane_local_debug.h"
        ),
        isoplane_abi::local_debug_header(),
    )
}
