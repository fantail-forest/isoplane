//! The epoch watchdog. Bumps the engine's epoch every `tick` to deliver
//! deadline kills, and sleeps on a condvar whenever nothing is hot so an idle
//! cell's CPU is not a spinning ticker. There is no yielding: wasmtime's
//! epoch-yield needs fibers, which are not compiled in.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::{Condvar, Mutex};

pub(crate) struct Watchdog {
    shared: Arc<Shared>,
    join: Option<std::thread::JoinHandle<()>>,
}

pub(crate) struct Shared {
    /// Dispatches currently hot in this engine. The ticker runs only while
    /// this is non-zero.
    hot: Mutex<u64>,
    wake: Condvar,
    stop: AtomicBool,
    tick: Duration,
}

impl Shared {
    #[cfg(test)]
    pub(crate) fn hot_count(&self) -> u64 {
        *self.hot.lock()
    }

    fn stop(&self) {
        // Synchronize with the idle check so its wait cannot miss this wake.
        let _hot = self.hot.lock();
        self.stop.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    /// Only the 0 to 1 transition touches the condvar.
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
}

impl Watchdog {
    pub(crate) fn start(engine: wasmtime::Engine, tick_us: u64) -> crate::Result<Self> {
        let tick = Duration::from_micros(tick_us.max(1));
        let shared = Arc::new(Shared {
            hot: Mutex::new(0),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            tick,
        });

        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("wisp-epoch".into())
            .spawn(move || run(engine, s))
            .context("spawning the epoch watchdog thread")?;

        Ok(Self {
            shared,
            join: Some(join),
        })
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(j) = self.join.take() {
            if j.join().is_err() {
                tracing::error!("the epoch watchdog thread panicked");
            }
        }
    }
}

fn run(engine: wasmtime::Engine, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Acquire) {
        {
            let mut hot = shared.hot.lock();
            while *hot == 0 && !shared.stop.load(Ordering::Acquire) {
                shared.wake.wait(&mut hot);
            }
        }
        if shared.stop.load(Ordering::Acquire) {
            break;
        }

        // An absolute schedule so error does not accumulate. `thread::sleep`
        // alone cannot hold a sub-millisecond cadence; sleeping to just short
        // of the deadline and spinning the remainder does, at a bounded CPU
        // cost paid only while a dispatch is hot.
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_an_idle_watchdog_releases_its_waiter() {
        let watchdog =
            Watchdog::start(wasmtime::Engine::default(), 200).expect("the watchdog must start");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            drop(watchdog);
            done_tx
                .send(())
                .expect("the shutdown observer must remain available");
        });
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("an idle watchdog must stop without losing its wake notification");
        join.join().expect("watchdog shutdown must not panic");
    }
}
