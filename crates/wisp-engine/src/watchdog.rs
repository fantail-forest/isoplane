//! The epoch watchdog (spec.md §6.4).
//!
//! > *The engine's epoch counter (bumped every 200 µs; checks Cranelift-inserted
//! > at back-edges/calls) exists to **deliver** deadline kills, not to
//! > deschedule-and-resume: a dispatch over budget is trapped and terminated
//! > without unwinding.*
//!
//! Two design points that are easy to get wrong:
//!
//! * **There is no yielding.** wasmtime can also use epochs to *yield* a
//!   running guest and resume it later, but that is implemented with fibers —
//!   hidden native stacks — which invariant 3 forbids. The `async` feature is
//!   not even compiled in (see `Cargo.toml`), so the option does not exist.
//! * **The watchdog sleeps when nothing is Hot.** A free-running 200 µs ticker
//!   burns a fraction of a core forever, which would put a floor under the
//!   cell's idle CPU and quietly contradict G2 ("an instance parked on I/O
//!   consumes zero CPU"). The ticker therefore blocks on a condvar whenever no
//!   dispatch is in flight and is woken by the first dispatch to go Hot.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

pub(crate) struct Watchdog {
    shared: Arc<Shared>,
    join: Option<std::thread::JoinHandle<()>>,
}

pub(crate) struct Shared {
    /// Number of dispatches currently Hot in this engine. The ticker runs only
    /// while this is non-zero.
    hot: Mutex<u64>,
    wake: Condvar,
    stop: AtomicBool,
    /// Monotonic count of epoch bumps actually published, for reporting the
    /// tick the watchdog *achieves* rather than the one it was asked for.
    ticks: AtomicU64,
    tick: Duration,
}

impl Shared {
    /// Called on the execution-pool thread as a dispatch goes Hot. Only the
    /// 0 -> 1 transition touches the condvar, so the steady-state cost on the
    /// hot path is one uncontended mutex acquire.
    #[inline]
    pub(crate) fn enter_hot(&self) {
        let mut n = self.hot.lock();
        *n += 1;
        if *n == 1 {
            self.wake.notify_one();
        }
    }

    #[inline]
    pub(crate) fn exit_hot(&self) {
        let mut n = self.hot.lock();
        *n = n.saturating_sub(1);
    }

    pub(crate) fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }
}

impl Watchdog {
    pub(crate) fn start(engine: wasmtime::Engine, tick_us: u64) -> Self {
        let tick = Duration::from_micros(tick_us.max(1));
        let shared = Arc::new(Shared {
            hot: Mutex::new(0),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            tick,
        });

        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("wisp-epoch".into())
            .spawn(move || run(engine, s))
            .expect("spawn epoch watchdog");

        Self { shared, join: Some(join) }
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // Unblock the ticker if it is parked waiting for work.
        self.shared.wake.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run(engine: wasmtime::Engine, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Acquire) {
        // Park until something goes Hot. This is what keeps an idle cell's CPU
        // at zero (G2) instead of at "one spinning watchdog".
        {
            let mut hot = shared.hot.lock();
            while *hot == 0 && !shared.stop.load(Ordering::Acquire) {
                shared.wake.wait(&mut hot);
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            break;
        }

        // Tick on an absolute schedule so error does not accumulate. The spike
        // (`q3_epoch.rs`) established that `thread::sleep` alone cannot hold a
        // 200 µs cadence; sleeping to just short of the deadline and spinning
        // the remainder does, at a small and bounded CPU cost that is only paid
        // while a dispatch is actually Hot.
        let mut next = Instant::now() + shared.tick;
        while *shared.hot.lock() > 0 && !shared.stop.load(Ordering::Acquire) {
            let now = Instant::now();
            if next > now {
                let d = next - now;
                const SPIN_TAIL: Duration = Duration::from_micros(60);
                if d > SPIN_TAIL {
                    std::thread::sleep(d - SPIN_TAIL);
                }
                while Instant::now() < next {
                    std::hint::spin_loop();
                }
            } else {
                next = now;
            }
            next += shared.tick;
            engine.increment_epoch();
            shared.ticks.fetch_add(1, Ordering::Relaxed);
        }
    }
}
