use core::future::Future;
use core::hint::black_box;
use core::task::{Context, Poll, Waker};

use isoplane_guest::{
    log, Clock, Ctx, Handler, Level, Request, RequestExt, Response, UnsafeToTrap,
};

pub struct Misbehaver;

/// Spins without awaiting; the host must end it with a budget kill.
fn spin_forever() -> ! {
    let mut x: u64 = 1;
    loop {
        x = black_box(x)
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        black_box(x);
    }
}

/// Polls a host future without returning to the host, so the future can never
/// complete; the host must end it with a budget kill.
fn sync_over_async() -> ! {
    let mut fut = Box::pin(Clock::delay(1000));
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(_) => spin_forever(),
            Poll::Pending => {
                black_box(0u8);
            }
        }
    }
}

/// Keeps and dirties 1 MiB chunks without returning. With `mb == 0` it grows
/// until the configured memory ceiling refuses an allocation, which must trap.
/// With `mb > 0` it stops after `mb + 1` chunks and spins, so under a ceiling
/// that admits them the host must end it with a budget kill.
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
            // Still under the ceiling, so nothing trapped. Spin rather than
            // return a 200; the budget kill ends the dispatch.
            black_box(&kept);
            spin_forever();
        }
    }
}

/// Spins in chunks and awaits between them; it must see the await cancelled
/// and return inside its grace, a budget exit.
async fn cooperative_spin() -> Response {
    const CHUNK: u64 = 1 << 14;

    // Lets a test observe that destructors ran on a budget exit.
    struct Cleanup(&'static str);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            log(Level::Info, self.0);
        }
    }
    let _finally = Cleanup("misbehaver/cooperative: finally ran (budget exit)");

    let mut x: u64 = 1;
    loop {
        for _ in 0..CHUNK {
            x = black_box(x)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            black_box(x);
        }
        if Clock::delay(0).await.is_err() {
            return Response::text(
                503,
                "cooperative: cancelled at its next slice and unwound (budget exit)",
            );
        }
    }
}

/// Spins with the unsafe-to-trap bit pinned; the host must end it with a
/// budget kill all the same.
fn gc_pin_spin() -> ! {
    let _pin = UnsafeToTrap::pin();
    spin_forever()
}

/// Executes `unreachable`; the host must report a trap and discard the instance.
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
