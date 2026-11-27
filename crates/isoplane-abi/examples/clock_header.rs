#![forbid(unsafe_code)]
fn main() -> std::io::Result<()> {
    std::fs::write(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../guest-sdks/dotnet/native/isoplane_clock_quantum.h"
        ),
        isoplane_abi::pal::clock_header(),
    )
}
