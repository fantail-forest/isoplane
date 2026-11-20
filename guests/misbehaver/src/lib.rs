//! `misbehaver`: hostile and broken routes for the budget, memory and trap
//! tests.
//!
//! | route | what it does | what the tests expect |
//! |---|---|---|
//! | `/spin` | infinite loop, never awaits | trapped when its grace ends; no unwinding; instance discarded |
//! | `/cooperative` | the same loop in chunks, awaiting between them | returns inside its grace: `Drop` runs and the instance is reusable |
//! | `/gc_pin` | spins with the *unsafe-to-trap* bit pinned | trapped when its grace ends all the same |
//! | `/sync_over_async` | polls a host future in a loop without returning | can never be woken, since a wake needs a return to the host; ends like `/spin` |
//! | `/alloc_bomb` | grows memory past the configured memory ceiling | the allocation fails and the guest traps |
//! | `/trap` | explicit `unreachable` | trapped; instance discarded, never retried on the same memory |
//! | `/ok` | returns 200 | the control: the module itself is fine |
//!
//! `/spin` and `/cooperative` run the same loop, so the await between chunks
//! is the only difference between the two outcomes.

use core::future::Future;
use core::hint::black_box;
use core::task::{Context, Poll, Waker};

use isoplane_guest::{
    log, Clock, Ctx, Handler, Level, Request, RequestExt, Response, UnsafeToTrap,
};

pub struct Misbehaver;

/// Never returns. `black_box` keeps LLVM from deleting the loop.
fn spin_forever() -> ! {
    let mut x: u64 = 1;
    loop {
        x = black_box(x)
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        black_box(x);
    }
}

/// The sync-over-async pathology, written out honestly.
///
/// This creates a real host future (a 1-second deadline), then polls it in a
/// loop instead of returning `Pending`. The host delivers results only through
/// `isoplane_wake`, and `isoplane_wake` can only be called when the guest is *not*
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

/// Grow linear memory until the configured memory ceiling refuses (`instance_memory_max`,
/// and V3 caps the declared maximum at deploy time).
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

/// The cooperative spinner: the clean kill, demonstrated by a real guest.
///
/// It burns CPU in the same loop as `spin_forever`, in chunks, and awaits a
/// zero-length host delay between them. That await returns to the host, and a
/// guest learns its budget ran out only at the slice after it did: the next
/// host future it awaits resolves to `Cancelled`, and this returns -- an
/// ordinary Rust `return`, which runs every `Drop` between here and the
/// entrypoint. Nothing inside a slice can tell the guest its budget ran out,
/// because a value that changed while it ran would be a clock.
///
/// The chunk is the honest cost of cooperating: the guest notices only once a
/// chunk ends, and up to two chunks run after B (the one B landed in, and one
/// whose delay was issued before the cancellation was visible). Much larger
/// and the return would land past B+G on a slow machine, turning this into
/// `/spin`.
async fn cooperative_spin() -> Response {
    const CHUNK: u64 = 1 << 14;

    // A `Drop` that observably ran. `finally` blocks and disposals run on a
    // clean kill and do NOT on a hard one, and that difference has to be
    // visible from outside the sandbox or it is not a claim, just a comment.
    struct Cleanup(&'static str);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            log(Level::Info, self.0);
        }
    }
    let _finally = Cleanup("misbehaver/cooperative: finally ran (clean kill)");

    let mut x: u64 = 1;
    loop {
        for _ in 0..CHUNK {
            x = black_box(x)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            black_box(x);
        }
        if Clock::delay(0).await.is_err() {
            // The unwind. `_finally` drops on the way out of this frame.
            return Response::text(
                503,
                "cooperative: cancelled at its next slice and unwound (clean kill)",
            );
        }
    }
}

/// Pin the *unsafe-to-trap* bit and then refuse to cooperate.
///
/// A runtime layer sets this bit around GC phases so grace does not land
/// mid-collection. The platform honours it for grace *entry* and ignores it at
/// the hard deadline -- the difference between "do not interrupt me right now"
/// and "do not interrupt me". This route asserts the second reading and must
/// lose: hard kill at B+G and an evicted instance, exactly like `/spin`.
fn gc_pin_spin() -> ! {
    let _pin = UnsafeToTrap::pin();
    spin_forever()
}

/// An explicit wasm `unreachable`. No panic machinery, no allocation, nothing
/// the runtime could turn into a graceful error.
fn hard_trap() -> ! {
    #[cfg(target_arch = "wasm32")]
    core::arch::wasm32::unreachable();
    #[cfg(not(target_arch = "wasm32"))]
    unreachable!("explicit trap")
}

impl Handler for Misbehaver {
    fn new() -> Self {
        Misbehaver
    }

    fn handle(&'static self, req: Request, _ctx: Ctx) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/spin" => spin_forever(),
                "/cooperative" => cooperative_spin().await,
                "/gc_pin" => gc_pin_spin(),
                "/sync_over_async" => sync_over_async(),
                "/alloc_bomb" => {
                    let mb = req
                        .query_get("mb")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    alloc_bomb(mb)
                }
                "/trap" => hard_trap(),
                "/ok" => Response::text(200, "ok"),
                _ => Response::text(404, "no such route"),
            }
        }
    }
}

isoplane_guest::export_handler!(Misbehaver);
