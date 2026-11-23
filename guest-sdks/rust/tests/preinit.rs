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
    with_stub_host(|_| {
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
    });
}

#[cfg(feature = "json")]
#[test]
fn the_deployment_settings_are_read_with_a_sizing_probe_then_one_fill() {
    with_stub_host(|host| {
        let settings = std::collections::BTreeMap::from([
            (
                "FX_URL".to_string(),
                "https://rates.example/nzd".to_string(),
            ),
            ("QUOTE_TTL_MS".to_string(), "300000".to_string()),
        ]);
        let json = serde_json::to_vec(&settings).unwrap();
        host.set_runtime_config(json.clone());

        assert_eq!(
            isoplane_guest::init::runtime_settings(),
            settings,
            "the guest must see exactly the supplied settings"
        );

        let capacities: Vec<i64> = host
            .calls()
            .iter()
            .filter(|(name, _)| *name == "runtime_config_get")
            .map(|(_, arguments)| arguments[1])
            .collect();
        assert_eq!(
            capacities,
            vec![0, json.len() as i64],
            "a zero-length probe, then one fill of the reported length"
        );
    });
}
