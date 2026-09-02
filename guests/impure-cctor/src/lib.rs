//! `impure-cctor` — the pre-init deferral fixture: three init units, two pure
//! and one that touches `clock_now_coarse`.
//!
//! Unit ids are the **0-based position** in `init_units!`; the "no unit in
//! scope" sentinel is `-1` (`u32::MAX` host-side), not 0. The impure unit sits
//! in the *middle* on purpose, so the fixture also shows that the units after
//! a deferred one still run.
//!
//! ```text
//!   pass 1:  begin(0) deferred(0)=0  run PURE_TABLE  ok   begin(-1)
//!            begin(1) deferred(1)=0  run STARTED_AT
//!                                      -> clock_now_coarse is not
//!                                         init_visible -> TRAP
//!   host:    current unit was 1. Add 1 to the deferred set, throw the
//!            instance away, replay pre-init from scratch. Rollback is by
//!            replay -- nothing outside the sandbox was touched.
//!   pass 2:  begin(0) deferred(0)=0  run PURE_TABLE  ok   begin(-1)
//!            begin(1) deferred(1)=1  SKIP
//!            begin(2) deferred(2)=0  run ROUTES      ok   begin(-1)
//!            -> freeze template.  deferred_units == [1]
//!   runtime: first touch of STARTED_AT runs it, per instance, on the
//!            instance's own CPU budget.
//! ```
//!
//! `GET /init` reports, per instance, which units ran here rather than at
//! deploy time — so the deferral is asserted from outside, not assumed.

use core::future::Future;

use serde::Serialize;
use wisp_guest::{Clock, Ctx, Handler, Request, Response};

wisp_guest::deferred_lazy! {
    /// **Unit 0 — PURE.** Hashes nothing, calls nothing. Pre-runs into the
    /// template and costs every instance zero.
    static PURE_TABLE: Vec<u32> = { (0..64u32).map(|i| i * i).collect() };

    /// **Unit 1 — IMPURE.** `clock_now_coarse` is not `init_visible`, so the
    /// sealed init sandbox traps here. This is the unit the host must learn
    /// to defer, and the id it must report in `deferred_units`.
    static STARTED_AT: u64 = { Clock::now_ms() };

    /// **Unit 2 — PURE**, declared after the impure one. If pre-init stopped
    /// at the first trap instead of replaying, this would never be baked.
    static ROUTES: Vec<&'static str> = { vec!["/init"] };
}

#[derive(Serialize)]
struct InitReport {
    /// The id the host sees for each unit, so a failure report can name the
    /// same thing the control plane's `deferred_units` does.
    unit_ids: [(&'static str, i32); 3],
    /// 0 if it pre-ran into the template (the correct answer for a pure unit).
    pure_table_runs_in_instance: u32,
    pure_table_len: usize,
    /// 0 likewise: unit 3 is pure and must survive unit 2 being deferred.
    routes_runs_in_instance: u32,
    routes_len: usize,
    /// 1 after first touch, because it was deferred to per-instance execution.
    started_at_runs_in_instance: u32,
    started_at: u64,
    /// The whole point, as one boolean: exactly the impure unit ran here.
    deferral_worked: bool,
}

pub struct Impure;

impl Handler for Impure {
    fn new() -> Self {
        Impure
    }
    fn handle(&'static self, req: Request, _ctx: Ctx) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/init" => {
                    // Touch the deferred unit first so the counter is meaningful.
                    let started_at = *STARTED_AT.get();
                    let deferral_worked = PURE_TABLE.run_count() == 0
                        && ROUTES.run_count() == 0
                        && STARTED_AT.run_count() == 1;
                    let body = serde_json::to_vec(&InitReport {
                        unit_ids: [
                            ("PURE_TABLE", wisp_guest::init::unit_id(0)),
                            ("STARTED_AT", wisp_guest::init::unit_id(1)),
                            ("ROUTES", wisp_guest::init::unit_id(2)),
                        ],
                        pure_table_runs_in_instance: PURE_TABLE.run_count(),
                        pure_table_len: PURE_TABLE.get().len(),
                        routes_runs_in_instance: ROUTES.run_count(),
                        routes_len: ROUTES.get().len(),
                        started_at_runs_in_instance: STARTED_AT.run_count(),
                        started_at,
                        deferral_worked,
                    })
                    .unwrap_or_default();
                    Response::new(200, body).with_header("content-type", "application/json")
                }
                _ => Response::text(404, "no such route"),
            }
        }
    }
}

wisp_guest::export_handler!(
    Impure,
    units = wisp_guest::init_units![PURE_TABLE, STARTED_AT, ROUTES]
);
