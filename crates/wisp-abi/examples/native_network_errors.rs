#![forbid(unsafe_code)]
fn main() {
    print!("{}", wisp_abi::native_io::network_error::managed_errors());
}
