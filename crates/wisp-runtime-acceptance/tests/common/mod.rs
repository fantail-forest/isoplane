//! The harness of this crate: the acceptance guest on a real reactor, in
//! either runtime, and the proof that the guest was built on the pack under
//! test. A behaviour is expected of both runtimes, so `in_both_runtimes!`
//! writes one test per runtime from one body and a failure names the
//! runtime it happened in. The socket peers and the scripted resolvers live
//! beside it, included by the binaries that need them.

pub use wisp_fixtures::GuestRuntime;
use wisp_harness::{Reactor, ReturnedGuest};
use wisp_host::egress::{DnsError, HostEgressPolicy};
use wisp_host::{InstanceId, Resolver};

/// Writes `mod $name { fn native_aot() {..} fn mono() {..} }` from one body,
/// so `cargo test mono` or `cargo test sockets::` select what to run. Each
/// test keeps its own test attribute (`#[test]`, `#[tokio::test(..)]`) and
/// doc comment, applied to both functions; one invocation holds tests of one
/// shape, asynchronous or plain.
#[macro_export]
macro_rules! in_both_runtimes {
    ($($(#[$meta:meta])* async fn $name:ident($runtime:ident: GuestRuntime) $body:block)+) => {
        $(
            mod $name {
                use super::*;

                $(#[$meta])*
                async fn native_aot() {
                    let $runtime = GuestRuntime::NativeAot;
                    $body
                }

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
                fn native_aot() {
                    let $runtime = GuestRuntime::NativeAot;
                    $body
                }

                $(#[$meta])*
                fn mono() {
                    let $runtime = GuestRuntime::Mono;
                    $body
                }
            }
        )+
    };
}

/// A resolver a test with no name to resolve gives the reactor: reaching it
/// is the failure.
pub struct NoDns;

#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(&self, host: &str, _: u16) -> Result<Vec<std::net::SocketAddr>, DnsError> {
        panic!("the guest must not resolve {host}: this behaviour needs no host lookup")
    }
}

/// A reactor for behaviour that reaches neither the network nor a resolver.
pub fn reactor_without_egress(owner: u64) -> Reactor {
    Reactor::new(
        InstanceId(owner),
        HostEgressPolicy::new(Box::new(NoDns), 8, 1 << 20),
    )
}

/// The acceptance guest for `runtime`, built on demand against the
/// source-built pack the feed offers it.
pub fn guest(reactor: &Reactor, runtime: GuestRuntime) -> ReturnedGuest {
    ReturnedGuest::new(&wisp_fixtures::runtime_acceptance_for(runtime), reactor)
}
