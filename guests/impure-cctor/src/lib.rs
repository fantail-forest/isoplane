//! `impure-cctor` — the §4.4 deferral fixture.
//!
//! One pure initializer and one impure one, and nothing else. This is the
//! minimal case the pre-init pipeline has to get right:
//!
//! ```text
//!   pass 1:  init_unit_begin(0); deferred(0)=0; run PURE_TABLE       -> ok
//!            init_unit_begin(1); deferred(1)=0; run STARTED_AT
//!                                                 -> clock_now_coarse
//!                                                 -> NOT init_visible
//!                                                 -> TRAP
//!   host:    unit 1 was running. Record it as deferred. Throw the instance
//!            away and REPLAY pre-init -- that is the rollback (§4.4).
//!   pass 2:  begin(0); deferred(0)=0; run PURE_TABLE  -> ok
//!            begin(1); deferred(1)=1; SKIP
//!            freeze template
//!   runtime: first touch of STARTED_AT runs it, per instance, on the
//!            instance's own CPU budget.
//! ```
//!
//! `/init` reports which of the two actually ran in this instance, so the
//! host-side test can assert the deferral happened rather than assuming it.

use core::future::Future;

use serde::Serialize;
use wisp_guest::{Clock, Ctx, Handler, Request, Response};

wisp_guest::deferred_lazy! {
    /// PURE: hashes nothing, calls nothing. Pre-runs into the template.
    static PURE_TABLE: Vec<u32> = { (0..64u32).map(|i| i * i).collect() };

    /// IMPURE: `clock_now_coarse` is not `init_visible`, so the sealed init
    /// sandbox traps here. This is the unit the host must learn to defer.
    static STARTED_AT: u64 = { Clock::now_ms() };
}

#[derive(Serialize)]
struct InitReport {
    /// 0 if it pre-ran into the template (the correct answer).
    pure_table_runs_in_instance: u32,
    pure_table_len: usize,
    /// 1 after first touch, because it was deferred to per-instance execution.
    started_at_runs_in_instance: u32,
    started_at: u64,
}

pub struct Impure;

impl Handler for Impure {
    fn new() -> Self {
        Impure
    }
    fn handle(
        &'static self,
        req: Request,
        _ctx: Ctx,
    ) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/init" => {
                    // Touch the deferred unit first so the counter is meaningful.
                    let started_at = *STARTED_AT.get();
                    let body = serde_json::to_vec(&InitReport {
                        pure_table_runs_in_instance: PURE_TABLE.run_count(),
                        pure_table_len: PURE_TABLE.get().len(),
                        started_at_runs_in_instance: STARTED_AT.run_count(),
                        started_at,
                    })
                    .unwrap_or_default();
                    Response::new(200, body).with_header("content-type", "application/json")
                }
                _ => Response::text(404, "no such route"),
            }
        }
    }
}

wisp_guest::export_handler!(Impure, units = wisp_guest::init_units![PURE_TABLE, STARTED_AT]);
