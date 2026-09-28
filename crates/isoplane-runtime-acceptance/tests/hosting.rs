//! The Microsoft.Extensions stack a customer's application is built on,
//! inside the guest: dependency injection and its scope guards,
//! configuration from memory and JSON (and its file providers on the empty
//! disk), options, logging into a sink the route owns, activities and
//! diagnostic listeners, the generic host's lifecycle, and the process
//! facts the runtime reports.

#[macro_use]
mod common;

use common::{guest, GuestRuntime};

async fn answer(runtime: GuestRuntime, owner: u64, path: &str) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(owner));
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_mono_runtime! {
    #[tokio::test]
    async fn services_resolve_with_singleton_scoped_transient_and_open_generic_lifetimes(runtime: GuestRuntime) {
        let ints = "Int32";
        assert_eq!(
            answer(runtime, 340, "/hosting/di").await,
            format!("tick 1 String clocks=1 same-greeter=True | tick 2 String clocks=1 new-greeter=True | ints={ints}"),
            "one singleton clock must count across two scopes, each scope its own greeter, the open generic must close over a reference type, and over a value type {runtime} must answer {ints}"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn the_container_refuses_a_scoped_service_from_the_root_and_an_unregistered_one(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 341, "/hosting/di-guards").await,
            "scoped-from-root=InvalidOperationException unregistered=InvalidOperationException optional=null validate-on-build=AggregateException",
            "scope validation, a required service that is missing, and validate-on-build over an unsatisfiable constructor must each refuse explicitly"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn in_memory_configuration_layers_sections_and_binds_to_options(runtime: GuestRuntime) {
        let hidden = "True/3";
        assert_eq!(
            answer(runtime, 342, "/hosting/config-memory").await,
            format!("on|9443|7|true/True|edge:9443:True:a.example,b.example|Hosts,Name,Port,Tls|False|hidden={hidden}"),
            "configuration must layer and bind generated options; reflection-only private setters must bind in the untrimmed interpreted guest"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn json_configuration_from_a_stream_binds_nested_sections_and_arrays(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 343, "/hosting/config-json").await,
            "json-edge:8080:False:j.example|value|z|3",
            "a JSON stream must bind objects, nested keys and arrays"
        );
    }
}

in_mono_runtime! {
    /// File providers meet the empty read-only disk: a required file fails
    /// as `FileNotFoundException`, an optional one is skipped, and the
    /// environment provider sees nothing.
    #[tokio::test]
    async fn file_backed_configuration_on_the_empty_disk_is_absent_or_refused(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 344, "/hosting/config-files").await,
            "required=FileNotFoundException optional=built relative-optional=built environment=0 base-directory=/",
            "a required file on the empty disk must fail by name, an optional one must build, and the environment must be empty"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn options_bind_post_configure_name_and_snapshot_per_scope(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 345, "/hosting/options").await,
            "opt:11|named:0|opt:11|same-singleton=True",
            "IOptions must bind then post-configure, a named instance must take only its own configuration, and the snapshot must match"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn logging_reaches_a_provider_with_structure_scopes_and_filters(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 346, "/hosting/logging").await,
            [
                "Information Acceptance.Hosting 0 handled /x in 3 ms [Path=/x,Ms=3] {request 42}",
                "Warning Acceptance.Hosting 7 budget 50 exceeded by inst-1 [Budget=50,Instance=inst-1] {request 42}",
                "Error Quiet 0 kept error [Why=error] {} InvalidOperationException",
            ]
            .join("\n"),
            "structured state, scopes, the source-generated message and the level and category filters must all reach the provider"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn activities_start_under_a_listener_with_w3c_ids_and_parenting(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 347, "/hosting/activity").await,
            "started=dispatch,stage|id-format=w3c|child-parent=parent|shared-trace=True|current-restored=True|after=none|unlistened=null",
            "a listened source must start W3C activities that nest, restore Activity.Current, and an unlistened source must start none"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_diagnostic_listener_delivers_only_subscribed_events_while_subscribed(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 348, "/hosting/diagnostic-listener").await,
            "seen=Kept.Event=payload enabled-after=False",
            "the predicate must gate events, and nothing must be delivered or enabled after the subscription is disposed"
        );
    }
}

in_mono_runtime! {
    /// The generic host built without defaults starts and stops its hosted
    /// service on the one thread, with no console lifetime, file provider
    /// or signal handler.
    #[tokio::test]
    async fn the_generic_host_without_defaults_runs_a_hosted_service_through_start_and_stop(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 349, "/hosting/host").await,
            "started,running,stopped greeting=hi app=acceptance env=Production root='/'",
            "the empty application builder must start and stop the hosted service and expose its configuration and environment"
        );
    }
}

in_mono_runtime! {
    /// The default application builder wires the console lifetime, the
    /// physical file provider over the content root, environment variables
    /// and the console logger: what of that the empty disk and the single
    /// thread refuse is recorded here.
    #[tokio::test]
    async fn the_default_application_builder_is_refused_by_what_it_assumes_of_the_process(runtime: GuestRuntime) {
        let answer = answer(runtime, 350, "/hosting/host-defaults").await;
        assert!(
            answer.starts_with("started,running,stopped"),
            "the default builder must run the hosted service through start and stop: {answer}"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn the_stopwatch_is_high_resolution_in_nanoseconds_over_the_hosts_clock(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, 351, "/hosting/stopwatch").await,
            "high-resolution=True frequency=1000000000 monotonic=True elapsed-non-negative=True tick-count-positive=True",
            "Stopwatch must report the Unix nanosecond frequency and never run backwards over the frozen slice clock"
        );
    }
}

in_mono_runtime! {
    /// One thread, a 32-bit wasm process, an empty environment, no
    /// arguments, and the runtime identifies itself.
    #[tokio::test]
    async fn the_process_facts_describe_a_single_threaded_wasm_guest(runtime: GuestRuntime) {
        let answer = answer(runtime, 352, "/hosting/environment").await;
        let expected_dynamic_code = "True";
        let mut facts = answer.split('|');
        for expected in [
            "cpus=1".to_string(),
            "64bit=False".to_string(),
            "platform=Other".to_string(),
            "arch=Wasm".to_string(),
            "rid=wasi-wasm".to_string(),
        ] {
            let fact = facts.next().unwrap_or_default();
            assert_eq!(fact, expected, "the process facts must describe a 32-bit single-threaded wasm guest: {answer}");
        }
        assert!(
            answer.contains("|version=10|"),
            "the runtime must be .NET 10: {answer}"
        );
        assert!(
            answer.contains(&format!("|dynamic-code={expected_dynamic_code}|")),
            "the interpreter must report dynamic code support: {answer}"
        );
        assert!(
            answer.contains("|variables=0|args=0|"),
            "the environment must be empty and the command line bare: {answer}"
        );
    }
}
