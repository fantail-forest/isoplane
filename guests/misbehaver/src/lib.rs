//! `misbehaver` — hostile and broken routes, for the §6.4/§8.3 kill tests.
//!
//! Every route here is supposed to die. What matters is *how*:
//!
//! | route | what it does | expected platform response |
//! |---|---|---|
//! | `/spin` | infinite loop, never awaits, never checks | **hard** kill at B+G; no unwinding (§2.5.8); instance tombstoned (§6.4) |
//! | `/cooperative` | the same loop, but it reads the remaining budget | **clean** kill inside grace: `Drop` runs, slot reusable (§6.4) |
//! | `/gc_pin` | spins with the *unsafe-to-trap* bit pinned | **hard** kill at B+G anyway — a pinned bit buys exactly G (§4.6) |
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
//!
//! # This module is the negative control for the two-phase kill
//!
//! §6.4 offers a grace window and claims clean kills are "the overwhelmingly
//! common case". A platform can make that claim look true by weakening the
//! hard path -- extending deadlines, forgiving overage, treating a spin as a
//! slow return. `/spin` exists so that it cannot: it is genuinely
//! uncooperative, it never calls `budget_remaining_ns`, it has no await point,
//! and if the hard trap at exactly B+G ever stops firing there is nothing left
//! to catch it. `/cooperative` is the same loop with one check added, so the
//! ONLY difference between a clean kill and a tombstone is guest behaviour.
//!
//! `/gc_pin` closes the remaining hole. §4.6 lets a runtime layer defer grace
//! while its *unsafe-to-trap* bit is set, and a deferral that could be held
//! indefinitely would be an escape hatch from the budget itself. This route
//! pins the bit and spins; it must still die at B+G.

use core::future::Future;
use core::hint::black_box;
use core::task::{Context, Poll, Waker};

use wisp_guest::{log, Budget, Clock, Ctx, Handler, Level, Request, RequestExt, Response, UnsafeToTrap};

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

/// The cooperative spinner: §6.4's clean kill, demonstrated by a real guest.
///
/// It burns CPU in exactly the same loop as `spin_forever`, with one addition:
/// every `CHECK_EVERY` iterations it reads the host-published remaining
/// budget. Once that goes non-positive the dispatch is inside its grace window
/// and this returns -- an ordinary Rust `return`, which runs every `Drop`
/// between here and the entrypoint. That return is the whole of the "managed
/// unwind" §2.5.8 promises; there is nothing else to it.
///
/// (Guests build with `panic = "abort"`, so `Drop`-on-return is the only
/// unwind shape there is. It is also the only one the platform needs: what a
/// clean kill buys is a consistent heap at an empty stack, and a `return`
/// produces exactly that.)
///
/// The check interval is the honest cost of cooperating: one host call per
/// 64k iterations of arithmetic. Much larger and the return would land past
/// B+G on a slow machine, turning this into `/spin` -- which is precisely the
/// failure the "tombstones at exactly B+G" conformance test is shaped to
/// catch, and why the interval is a constant a reader can see rather than a
/// tuned number.
fn cooperative_spin() -> Response {
    const CHECK_EVERY: u64 = 1 << 16;

    // A `Drop` that observably ran. §2.5.8: `finally` blocks and disposals run
    // on a clean kill and do NOT on a hard one, and that difference has to be
    // visible from outside the sandbox or it is not a claim, just a comment.
    struct Cleanup(&'static str);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            log(Level::Info, self.0);
        }
    }
    let _finally = Cleanup("misbehaver/cooperative: finally ran (§2.5.8, clean kill)");

    let mut x: u64 = 1;
    let mut n: u64 = 0;
    loop {
        x = black_box(x).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        black_box(x);
        n += 1;
        if n % CHECK_EVERY == 0 && Budget::exhausted() {
            // The unwind. `_finally` drops on the way out of this frame.
            return Response::text(
                503,
                "cooperative: observed the grace window and unwound (§6.4 clean kill)",
            );
        }
    }
}

/// Pin §4.6's *unsafe-to-trap* bit and then refuse to cooperate.
///
/// A runtime layer sets this bit around GC phases so grace does not land
/// mid-collection. The platform honours it for grace *entry* and ignores it at
/// the hard deadline -- the difference between "do not interrupt me right now"
/// and "do not interrupt me". This route asserts the second reading and must
/// lose: hard kill at B+G and a tombstone, exactly like `/spin`.
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

    fn handle(
        &'static self,
        req: Request,
        _ctx: Ctx,
    ) -> impl Future<Output = Response> + 'static {
        async move {
            match req.path.as_str() {
                "/spin" => spin_forever(),
                "/cooperative" => cooperative_spin(),
                "/gc_pin" => gc_pin_spin(),
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
