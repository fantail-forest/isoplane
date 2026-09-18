#![forbid(unsafe_code)]
fn main() {
    print!(
        "{}",
        isoplane_abi::native_io::network_error::managed_errors()
    );
}
