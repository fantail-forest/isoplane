//! Template initialization: what the application's initializer builds, and
//! what is left for each instance to build on first use.

use std::sync::atomic::{AtomicU32, Ordering};

use isoplane_guest::testing::with_stub_host;
use isoplane_guest::{Ctx, Handler, Request, Response};

static FORCED_RAN: AtomicU32 = AtomicU32::new(0);
static UNFORCED_RAN: AtomicU32 = AtomicU32::new(0);

isoplane_guest::lazy_init! {
    /// Forced by the initializer, so it lands in the template.
    static FORCED: u32 = {
        FORCED_RAN.fetch_add(1, Ordering::Relaxed);
        7
    };
    /// Left to each instance.
    static UNFORCED: u32 = {
        UNFORCED_RAN.fetch_add(1, Ordering::Relaxed);
        11
    };
}

struct App;

impl Handler for App {
    fn new() -> Self {
        App
    }
    async fn handle(&'static self, _r: Request, _c: Ctx) -> Response {
        Response::status(204)
    }
}

isoplane_guest::export_handler!(
    App,
    initialize = || {
        FORCED.force();
    }
);

#[test]
fn the_initializer_builds_what_it_forces_and_the_rest_is_built_on_first_touch() {
    with_stub_host(|host| {
        isoplane_initialize();

        assert_eq!(
            FORCED_RAN.load(Ordering::Relaxed),
            1,
            "the initializer builds a value it forces"
        );
        assert!(FORCED.is_initialized());
        assert_eq!(
            UNFORCED_RAN.load(Ordering::Relaxed),
            0,
            "a value the initializer does not force is not built during initialization"
        );
        assert!(!UNFORCED.is_initialized());

        assert_eq!(*UNFORCED.get(), 11);
        assert_eq!(*UNFORCED.get(), 11);
        assert_eq!(
            UNFORCED.run_count(),
            1,
            "a value is built once, on first touch"
        );
        assert_eq!(*FORCED.get(), 7);
        assert_eq!(
            FORCED.run_count(),
            1,
            "touching a value the initializer built does not rebuild it"
        );

        // The host answers the initialization imports with traps once an
        // instance serves, so the SDK serves the captured seed afterwards.
        let seed_reads = || {
            host.calls()
                .iter()
                .filter(|(n, _)| *n == "init_hash_seed")
                .count()
        };
        let before = seed_reads();
        let _ = isoplane_guest::init::hash_seed();
        assert_eq!(
            before,
            seed_reads(),
            "hash_seed must not call its import after initialization"
        );
    });
}

/// Config parsing is one of the things a template deletes from cold
/// activation. The blob is tenant-authored deploy input, so parsing it during
/// initialization puts the *parsed* result in the template. This drives the real probe-then-fill idiom against a host stub
/// that mirrors `isoplane_prepare::preinit`.
#[cfg(feature = "json")]
#[test]
fn config_is_readable_at_init_and_only_at_init() {
    with_stub_host(|host| {
        let vars = vec![
            (
                "FX_URL".to_string(),
                "https://rates.example/nzd".to_string(),
            ),
            ("QUOTE_TTL_MS".to_string(), "300000".to_string()),
        ];
        host.set_config(serde_json::to_vec(&vars).unwrap());

        let got = isoplane_guest::init::config_vars();
        assert_eq!(got, vars, "the guest must see exactly the declared vars");
        assert_eq!(
            isoplane_guest::init::config_get("QUOTE_TTL_MS").as_deref(),
            Some("300000")
        );
        assert!(isoplane_guest::init::config_is_bound());

        // Probe (cap 0) then fill (cap = full length): two calls, not one.
        let calls: Vec<i64> = host
            .calls()
            .iter()
            .filter(|(n, _)| *n == "init_config_get")
            .map(|(_, a)| a[1])
            .collect();
        assert_eq!(calls.len(), 2, "zero-length probe, then one fill");
        assert_eq!(calls[0], 0, "the probe must not claim a buffer it lacks");
        assert!(calls[1] > 0);

        // Cached: later reads must not touch the import again, because the
        // runtime linker binds init-only imports to traps.
        let before = host
            .calls()
            .iter()
            .filter(|(n, _)| *n == "init_config_get")
            .count();
        let _ = isoplane_guest::init::config_vars();
        let after = host
            .calls()
            .iter()
            .filter(|(n, _)| *n == "init_config_get")
            .count();
        assert_eq!(
            before, after,
            "config must be served from the template after init"
        );
    });
}
