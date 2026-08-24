//! Handler fixture for initialization, host timer parking and streamed responses.

use core::cell::Cell;

use serde::Serialize;
use wisp_guest::{Clock, Ctx, Handler, Request, RequestExt, Response};

wisp_guest::deferred_lazy! {
    /// Routes handled by this fixture.
    static ROUTES: Vec<&'static str> = { vec!["/memo", "/park", "/stream"] };

    /// Reads the serving clock when this instance first needs it.
    static BOOT_MS: u64 = { Clock::now_ms() };
}

/// Handles initialization, timer and streaming probes.
pub struct Api {
    dispatches: Cell<u64>,
}

#[derive(Serialize)]
struct MemoReport {
    /// Dispatches this *instance* has served. Stays at 1 forever under
    /// `fresh`; climbs under `reuse`.
    instance_dispatches: u64,
    /// Initializer executions represented by this guest state, including preparation.
    routes_initializer_runs: u32,
    routes_len: usize,
    /// Initializer executions before this report reads the clock value.
    boot_ms_initializer_runs: u32,
    boot_ms: u64,
    heap_live_bytes: usize,
    heap_brk: usize,
}

#[derive(Serialize)]
struct ParkReport {
    requested_ms: u64,
    /// Pending returns observed by this instance.
    parks: u64,
    polls: u64,
}

impl Api {
    fn memo_report(&'static self) -> Response {
        let h = wisp_guest::heap_stats();
        Response::json(&MemoReport {
            instance_dispatches: self.dispatches.get(),
            routes_initializer_runs: ROUTES.run_count(),
            routes_len: ROUTES.get().len(),
            boot_ms_initializer_runs: BOOT_MS.run_count(),
            boot_ms: *BOOT_MS.get(),
            heap_live_bytes: h.live_bytes,
            heap_brk: h.brk,
        })
    }

    async fn park(&'static self, req: Request, ctx: Ctx) -> Response {
        let ms: u64 = req
            .query_get("ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        // A timer is a host future, never a guest loop (§5.3). This `.await`
        // is a `return Poll::Pending` all the way out to the host.
        let _ = Clock::delay(ms).await;
        Response::json(&ParkReport {
            requested_ms: ms,
            parks: ctx.park_count(),
            polls: ctx.poll_count(),
        })
    }

    async fn stream(&'static self) -> Response {
        let s = wisp_guest::ResponseStream::open(
            200,
            vec![("content-type".into(), "text/plain".into())],
        );
        for i in 0..3u32 {
            if let Err(e) = s.write(format!("chunk {i}\n").as_bytes()).await {
                wisp_guest::log(wisp_guest::Level::Warn, &format!("stream write: {e}"));
                break;
            }
        }
        let r = Response::from_stream(&s);
        s.finish();
        r
    }
}

impl Handler for Api {
    fn new() -> Self {
        Api {
            dispatches: Cell::new(0),
        }
    }

    async fn handle(&'static self, req: Request, ctx: Ctx) -> Response {
        self.dispatches.set(self.dispatches.get() + 1);
        match req.path.as_str() {
            "/memo" => self.memo_report(),
            "/park" => self.park(req, ctx).await,
            "/stream" => self.stream().await,
            "/routes" => Response::text(200, ROUTES.get().join(",")),
            _ => Response::text(404, "no such route"),
        }
    }
}

wisp_guest::export_handler!(Api, units = wisp_guest::init_units![ROUTES, BOOT_MS]);
