//! Pre-initialization: which initializers run inside the sealed init sandbox,
//! and what happens to the ones that cannot.
//!
//! The protocol under test is the replay handshake:
//!
//! * `isoplane_initialize` announces every unit with `init_unit_begin(id)` BEFORE
//!   running it, so a trap inside the unit identifies itself;
//! * on the replay, `init_unit_deferred(id)` tells the guest to skip the unit
//!   the host learned was impure;
//! * a skipped unit runs lazily, per instance, on first touch -- ordinary
//!   .NET lazy-cctor semantics.

use std::sync::atomic::{AtomicU32, Ordering};

use isoplane_guest::testing::with_stub_host;
use isoplane_guest::{Ctx, Handler, Request, Response};

static PURE_RAN: AtomicU32 = AtomicU32::new(0);
static IMPURE_RAN: AtomicU32 = AtomicU32::new(0);
static AFTER_RAN: AtomicU32 = AtomicU32::new(0);

isoplane_guest::deferred_lazy! {
    /// Unit **0**. Pure: pre-runs into the template.
    static PURE: u32 = {
        PURE_RAN.fetch_add(1, Ordering::Relaxed);
        7
    };
    /// Unit **1**. Stands in for an initializer that touches a forbidden
    /// import and therefore TRAPS in the sealed init sandbox.
    static IMPURE: u32 = {
        IMPURE_RAN.fetch_add(1, Ordering::Relaxed);
        11
    };
    /// Unit **2**. Pure, and declared *after* the impure one, so this fixture
    /// also proves the loop keeps going past a deferred unit.
    static AFTER: u32 = {
        AFTER_RAN.fetch_add(1, Ordering::Relaxed);
        13
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
    units = isoplane_guest::init_units![PURE, IMPURE, AFTER]
);

#[test]
fn a_deferred_unit_is_skipped_at_init_and_runs_on_first_touch() {
    with_stub_host(|host| {
        // The host has already discovered, on a previous pass, that unit 1
        // traps. This replay must skip it. Ids are the 0-based position in
        // `init_units!`; the "no unit in scope" sentinel is -1 (`u32::MAX`
        // host-side), NOT 0 -- 0 is a real unit.
        host.defer_unit(1);

        isoplane_initialize();

        let calls = host.calls();
        let begins: Vec<i64> = calls
            .iter()
            .filter(|(n, _)| *n == "init_unit_begin")
            .map(|(_, a)| a[0])
            .collect();
        // 0 runs (then leaves scope with the -1 sentinel), 1 is skipped (the
        // host clears its own current-unit on a deferred answer, so no
        // sentinel is needed there), 2 runs.
        assert_eq!(
            begins,
            vec![0, -1, 1, 2, -1],
            "0-based ids; `begin(NO_UNIT)` only after a unit that actually ran"
        );
        assert_eq!(isoplane_guest::init::NO_UNIT, -1);

        assert_eq!(PURE_RAN.load(Ordering::Relaxed), 1, "a pure unit pre-runs");
        assert_eq!(
            AFTER_RAN.load(Ordering::Relaxed),
            1,
            "a deferred unit must not stop the units after it"
        );
        assert_eq!(
            IMPURE_RAN.load(Ordering::Relaxed),
            0,
            "a deferred unit must NOT run inside the init sandbox"
        );
        assert!(PURE.is_initialized());
        assert!(!IMPURE.is_initialized());

        // ...and it runs, per instance, on first touch.
        assert_eq!(*IMPURE.get(), 11);
        assert_eq!(IMPURE_RAN.load(Ordering::Relaxed), 1);
        assert_eq!(IMPURE.run_count(), 1);

        // Under `lifecycle = "reuse"` that cost is paid once per instance,
        // not once per dispatch. Touching it again must not re-run it.
        assert_eq!(*IMPURE.get(), 11);
        assert_eq!(IMPURE.run_count(), 1);

        // A pre-run unit reports 0 in-instance runs, because it never ran in
        // an instance -- it ran at deploy time. `/memo` in `quotes-api`
        // surfaces exactly this number.
        assert_eq!(PURE.run_count(), 1, "this test IS the init sandbox pass");

        // Init-only imports stay declared in the template but the runtime
        // linker binds them to traps. After `isoplane_initialize` the SDK must
        // serve the captured seed rather than call the import again.
        let before = host
            .calls()
            .iter()
            .filter(|(n, _)| *n == "init_hash_seed")
            .count();
        let _ = isoplane_guest::init::hash_seed();
        let after = host
            .calls()
            .iter()
            .filter(|(n, _)| *n == "init_hash_seed")
            .count();
        assert_eq!(
            before, after,
            "hash_seed must not touch an init-only import after init"
        );
    });
}

/// Config parsing is one of the things a template deletes from cold
/// activation. The blob is tenant-authored deploy input, so parsing it inside
/// an init unit is safe to pre-run and the *parsed* result is what lands in the
/// template. This drives the real probe-then-fill idiom against a host stub
/// that mirrors `isoplane_control::preinit`.
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
