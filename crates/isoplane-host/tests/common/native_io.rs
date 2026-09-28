//! The runtime-parameterised test macro shared by native I/O tests: runs one
//! test body under the Mono runtime, on the real reactor from
//! `isoplane_harness`.

pub use isoplane_fixtures::GuestRuntime;
pub use isoplane_harness::{Reactor, ReturnedGuest, WAKE_DEADLINE};

/// Writes `mod $name { async fn mono() {..} }`, preserving the body's test attributes.
macro_rules! in_mono_runtime {
    ($( $(#[$attribute:meta])* async fn $name:ident($runtime:ident: GuestRuntime) $body:block )*) => {
        $(
            mod $name {
                use super::*;

                $(#[$attribute])*
                async fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )*
    };
}
