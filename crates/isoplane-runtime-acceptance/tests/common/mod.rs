//! The source-built Mono acceptance guest on a real reactor, with pack identity verification.

pub use isoplane_fixtures::GuestRuntime;
use isoplane_harness::{Reactor, ReturnedGuest};

/// Writes `mod $name { fn mono() {..} }` from one body,
/// so `cargo test mono` or `cargo test sockets::` select what to run. Each
/// test keeps its own test attribute (`#[test]`, `#[tokio::test(..)]`) and
/// doc comment, applied to the function; one invocation holds tests of one
/// shape, asynchronous or plain.
#[macro_export]
macro_rules! in_mono_runtime {
    ($($(#[$meta:meta])* async fn $name:ident($runtime:ident: GuestRuntime) $body:block)+) => {
        $(
            mod $name {
                use super::*;

                $(#[$meta])*
                async fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )+
    };
    ($($(#[$meta:meta])* fn $name:ident($runtime:ident: GuestRuntime) $body:block)+) => {
        $(
            mod $name {
                use super::*;

                $(#[$meta])*
                fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )+
    };
}

/// The acceptance guest for `runtime`, built on demand against the
/// source-built pack the feed offers it.
pub fn guest(reactor: &Reactor, runtime: GuestRuntime) -> ReturnedGuest {
    ReturnedGuest::new(&isoplane_fixtures::runtime_acceptance_for(runtime), reactor)
}
