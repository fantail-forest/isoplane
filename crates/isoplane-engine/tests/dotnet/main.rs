//! Managed behaviour through real C# guests, in both execution runtimes.
//! One binary so the fixtures share one harness and one loaded template per
//! runtime; each module proves one family of behaviour.

#[path = "../common/bindings.rs"]
mod bindings;
#[path = "../common/dotnet.rs"]
#[macro_use]
mod dotnet;

mod activation;
mod certificates;
mod hashing;
mod managed_async;
mod randomness;
mod streaming;

/// The fixtures of both runtimes link the source-built packs the feed offers
/// now; the identities a result is attributable to are in the test output.
#[test]
fn the_fixtures_are_attributed_to_the_feeds_packs() {
    for runtime in isoplane_fixtures::GuestRuntime::ALL {
        let identity =
            isoplane_fixtures::dotnet_runtime_identity("isoplane_guest_dotnet_hashing", runtime);
        assert_eq!(
            identity,
            isoplane_fixtures::feed_runtime_identity(runtime),
            "a {runtime} fixture must be published against the pack the feed offers"
        );
        assert!(
            identity.contains("-isoplane-"),
            "the {runtime} runtime under test must be the source-built Isoplane pack: {identity}"
        );
        println!("{runtime} runtime under test: {identity}");
    }
}
