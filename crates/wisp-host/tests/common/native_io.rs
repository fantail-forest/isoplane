//! The runtime-parameterised test macro shared by native I/O tests: runs one
//! test body under both guest runtimes, on the real reactor from
//! `wisp_harness`.

pub use wisp_fixtures::GuestRuntime;
pub use wisp_harness::{Reactor, ReturnedGuest, WAKE_DEADLINE};

/// Writes `mod $name { async fn native_aot() {..} async fn mono() {..} }`
/// from one body, with the attributes (`#[tokio::test]`, paused time) on
/// both, so a behaviour is proven in each runtime and named by it.
macro_rules! in_both_runtimes {
    ($( $(#[$attribute:meta])* async fn $name:ident($runtime:ident: GuestRuntime) $body:block )*) => {
        $(
            mod $name {
                use super::*;

                $(#[$attribute])*
                async fn native_aot() {
                    let $runtime = GuestRuntime::NativeAot;
                    $body
                }

                $(#[$attribute])*
                async fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )*
    };
}
