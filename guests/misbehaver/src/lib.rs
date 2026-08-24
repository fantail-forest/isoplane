//! `misbehaver` — hostile and broken routes, for the §6.4/§8.3 kill tests.
//!
//! Every route here is supposed to die. What matters is *how*:
//!
//! | route | what it does | expected platform response |
//! |---|---|---|
//! | `/spin` | infinite loop, never awaits | killed at the CPU budget (§6.4); no unwinding (§2.5.8) |
//! | `/sync_over_async` | polls a host future in a loop without returning | **can never** be woken, so it dies at the cap (§4.5, §8.3) |
//! | `/alloc_bomb` | grows memory past the plan ceiling | allocation fails -> trap (§11.1, §V3) |
//! | `/trap` | explicit `unreachable` | trapped; instance discarded, never retried on the same memory (§13) |
//! | `/ok` | returns 200 | the control: the module itself is fine |
//!
//! §8.3's claim is that the blocking residue is *self-punishing*: a spinner
//! occupies only its own execution-pool thread, the kernel preempts it, and
//! the epoch-delivered budget kill ends it. `/sync_over_async` is the sharper
//! case -- it is not merely slow, it is **provably unable to make progress**,
//! because the only way to receive a wake is to return to the host, and it
//! never does. That is the whole of "sync-over-async can never complete".

use core::future::Future;
use core::hint::black_box;
use core::task::{Context, Poll, Waker};

use wisp_guest::{Clock, Ctx, Handler, Request, RequestExt, Response};

pub struct Misbehaver;

/// Never returns. `black_box` keeps LLVM from deleting the loop.
fn spin_forever() -> ! {
    let mut x: u64 = 1;
    loop {
        x = black_box(x).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        black_box(x);
    }
}

/// The sync-over-async pathology, written out honestly.
///
/// This creates a real host future (a 1-second deadline), then polls it in a
/// loop instead of returning `Pending`. The host delivers results only through
/// `wisp_wake`, and `wisp_wake` can only be called when the guest is *not*
/// executing -- so this loop can never observe a completion. It is not slow;
/// it is impossible. The CPU budget kill is what ends it.
fn sync_over_async() -> ! {
    let mut fut = Box::pin(Clock::delay(1000));
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            // Unreachable by construction: see above.
            Poll::Ready(_) => spin_forever(),
            Poll::Pending => {
                black_box(0u8);
            }
        }
    }
}

/// Grow linear memory until the plan ceiling refuses (§11.1
/// `instance_memory_max`, and V3 caps the declared maximum at deploy time).
/// The allocation failure becomes an abort, which is a wasm trap.
fn alloc_bomb(mb: usize) -> ! {
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut n = 0usize;
    loop {
        // Touch every page so the pages are really dirtied, not just reserved.
        let mut chunk = vec![0u8; 1 << 20];
        for p in chunk.chunks_mut(4096) {
            p[0] = 0xAB;
        }
        kept.push(chunk);
        n += 1;
        if mb != 0 && n > mb {
            // Even the bounded variant must not return: the test wants a
            // memory kill, not a 200.
            black_box(&kept);
            spin_forever();
        }
    }
}

/// An explicit wasm `unreachable`. No panic machinery, no allocation, nothing
/// the runtime could turn into a graceful error.
fn hard_trap() -> ! {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    unreachable!("explicit trap")
}

impl Handler for Misbehaver {
    fn new() -> Self {
        Misbehaver
    }

    fn handle(
        &'static self,
        req: Request,
        _ctx: Ctx,
    ) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/spin" => spin_forever(),
                "/sync_over_async" => sync_over_async(),
                "/alloc_bomb" => {
                    let mb = req.query_get("mb").and_then(|s| s.parse().ok()).unwrap_or(0);
                    alloc_bomb(mb)
                }
                "/trap" => hard_trap(),
                "/ok" => Response::text(200, "ok"),
                _ => Response::text(404, "no such route"),
            }
        }
    }
}

wisp_guest::export_handler!(Misbehaver);
