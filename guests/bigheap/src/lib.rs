//! A Handler fixture that allocates and retains heap blocks.
//! `/dirty?pages=N` allocates blocks; `/release?keep=N` releases blocks.
//! `/stats` reports guest heap state; `/park?ms=N` waits on a host timer while retaining blocks.

use core::cell::RefCell;
use core::future::Future;

use serde::Serialize;
use wisp_guest::{Clock, Ctx, Handler, Request, RequestExt, Response};

const PAGE: usize = 4096;

pub struct BigHeap {
    held: RefCell<Vec<Vec<u8>>>,
}

#[derive(Serialize)]
struct Report {
    blocks_held: usize,
    pages_held: usize,
    live_bytes: usize,
    free_bytes: usize,
    brk: usize,
    high_water: usize,
    arena_bytes: usize,
    last_tail_reclaimed: usize,
    last_zeroed: usize,
}

impl BigHeap {
    fn report(&'static self) -> Response {
        let h = wisp_guest::heap_stats();
        let held = self.held.borrow();
        Response::json(&Report {
            blocks_held: held.len(),
            pages_held: held.iter().map(|b| b.len() / PAGE).sum(),
            live_bytes: h.live_bytes,
            free_bytes: h.free_bytes,
            brk: h.brk,
            high_water: h.high_water,
            arena_bytes: h.arena_bytes,
            last_tail_reclaimed: h.last_tail_reclaimed,
            last_zeroed: h.last_zeroed,
        })
    }
}

impl Handler for BigHeap {
    fn new() -> Self {
        BigHeap { held: RefCell::new(Vec::new()) }
    }

    fn handle(
        &'static self,
        req: Request,
        _ctx: Ctx,
    ) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/dirty" => {
                    let pages: usize =
                        req.query_get("pages").and_then(|s| s.parse().ok()).unwrap_or(256);
                    let byte: u8 = req
                        .query_get("byte")
                        .and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                        .unwrap_or(0xAB);
                    // One allocation per 64 pages keeps the free list short
                    // while still exercising realloc/coalescing.
                    let mut block = vec![0u8; pages * PAGE];
                    for p in block.chunks_mut(PAGE) {
                        // Touch each page exactly once: this is what the
                        // host's soft-dirty walk will see.
                        p[0] = byte;
                    }
                    self.held.borrow_mut().push(block);
                    self.report()
                }
                "/release" => {
                    let keep: usize =
                        req.query_get("keep").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let mut held = self.held.borrow_mut();
                    while held.len() > keep {
                        held.pop();
                    }
                    drop(held);
                    self.report()
                }
                "/park" => {
                    let ms: u64 =
                        req.query_get("ms").and_then(|s| s.parse().ok()).unwrap_or(1000);
                    let _ = Clock::delay(ms).await;
                    self.report()
                }
                "/stats" => self.report(),
                _ => Response::text(404, "no such route"),
            }
        }
    }
}

wisp_guest::export_handler!(BigHeap);
