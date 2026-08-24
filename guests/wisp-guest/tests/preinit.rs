//! §4.4 pre-initialization: which initializers run inside the sealed init
//! sandbox, and what happens to the ones that cannot.
//!
//! The protocol under test is the replay handshake:
//!
//! * `wisp_initialize` announces every unit with `init_unit_begin(id)` BEFORE
//!   running it, so a trap inside the unit identifies itself;
//! * on the replay, `init_unit_deferred(id)` tells the guest to skip the unit
//!   the host learned was impure;
//! * a skipped unit runs lazily, per instance, on first touch -- ordinary
//!   .NET lazy-cctor semantics.

use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};

use wisp_guest::testing::with_stub_host;
use wisp_guest::{Ctx, Handler, Request, Response};

static PURE_RAN: AtomicU32 = AtomicU32::new(0);
static IMPURE_RAN: AtomicU32 = AtomicU32::new(0);

wisp_guest::deferred_lazy! {
    /// Unit 0. Pure: pre-runs into the template.
    static PURE: u32 = {
        PURE_RAN.fetch_add(1, Ordering::Relaxed);
        7
    };
    /// Unit 1. Stands in for an initializer that touches a forbidden import
    /// and therefore TRAPS in the sealed init sandbox.
    static IMPURE: u32 = {
        IMPURE_RAN.fetch_add(1, Ordering::Relaxed);
        11
    };
}

struct App;

impl Handler for App {
    fn new() -> Self {
        App
    }
    fn handle(&'static self, _r: Request, _c: Ctx) -> impl Future<Output = Response> + 'static {
        async { Response::status(204) }
    }
}

wisp_guest::export_handler!(App, units = wisp_guest::init_units![PURE, IMPURE]);

#[test]
fn a_deferred_unit_is_skipped_at_init_and_runs_on_first_touch() {
    with_stub_host(|host| {
        // The host has already discovered, on a previous pass, that unit 1
        // traps. This replay must skip it.
        host.defer_unit(1);

        wisp_initialize();

        let calls = host.calls();
        let begins: Vec<i64> =
            calls.iter().filter(|(n, _)| *n == "init_unit_begin").map(|(_, a)| a[0]).collect();
        assert_eq!(begins, vec![0, 1], "every unit must announce itself before running");

        assert_eq!(PURE_RAN.load(Ordering::Relaxed), 1, "a pure unit pre-runs");
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

        // §2.6: under `reuse` that cost is paid once per instance, not once
        // per dispatch. Touching it again must not re-run it.
        assert_eq!(*IMPURE.get(), 11);
        assert_eq!(IMPURE.run_count(), 1);

        // A pre-run unit reports 0 in-instance runs, because it never ran in
        // an instance -- it ran at deploy time. `/memo` in `quotes-api`
        // surfaces exactly this number.
        assert_eq!(PURE.run_count(), 1, "this test IS the init sandbox pass");
    });
}
