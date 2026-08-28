//! Contended lock-acquisition counts and elapsed wait time.
//! Shared counters are updated only after a failed immediate acquisition.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Counter-sampling epoch identifying measurement semantics.
/// Epoch 0 performs both counter updates while holding the lock. Epoch 1 moves
/// collision accounting before blocking and records eligible waits after release.
pub const EPOCH: u32 = 1;

/// A counter with a cacheline to itself.
///
/// 128 bytes rather than 64, matching `cell::Lane`: x86 prefetches lines in
/// pairs and aarch64 can have a 128-byte line, so 64 does not actually buy a
/// write the exclusive ownership it was asking for.
#[derive(Debug, Default)]
#[repr(align(128))]
struct Padded(AtomicU64);

/// Contended acquisitions of one lock, and the time they cost.
///
/// The two counters are deliberately a cacheline apart. They are written from
/// opposite sides of the same acquisition — `contended` by a thread that is
/// about to block, `blocked_ns` by the thread that just stopped blocking — and
/// on one line the second write would have to take a line the first writers
/// are queueing for, from inside a critical section. See the module header.
#[derive(Debug, Default)]
pub struct LockStats {
    /// Acquisitions whose first compare-and-swap collided.
    ///
    /// Deliberately not a count of *all* acquisitions: an uncontended one is
    /// free and uninteresting, and counting it would put a shared atomic on
    /// the fast path, which is the one thing this must not do.
    ///
    /// Read the header before dividing by it. This is an upper bound on the
    /// acquisitions that blocked, not a count of them.
    contended: Padded,
    /// Nanoseconds spent blocked, summed across threads.
    ///
    /// The figure to rank on. A lock taken often but never held long costs
    /// less than one taken rarely and held across a syscall, and a contended
    /// *count* cannot tell those apart.
    blocked_ns: Padded,
}

impl LockStats {
    /// One collision, counted where it is cheap: by a thread that holds
    /// nothing and is about to sleep.
    #[inline]
    fn collision(&self) {
        self.contended.0.fetch_add(1, Ordering::Relaxed);
    }

    /// What that collision turned out to cost.
    #[inline]
    fn add_wait(&self, ns: u64) {
        self.blocked_ns.0.fetch_add(ns, Ordering::Relaxed);
    }

    /// Both at once, for a caller that has already released the lock.
    #[inline]
    fn record(&self, ns: u64) {
        self.collision();
        self.add_wait(ns);
    }

    pub fn snapshot(&self) -> LockSample {
        LockSample {
            contended: self.contended.0.load(Ordering::Relaxed),
            blocked_ns: self.blocked_ns.0.load(Ordering::Relaxed),
        }
    }
}

/// One lock's figures at an instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockSample {
    /// Acquisitions whose first CAS collided — see [`LockStats::contended`],
    /// and in particular do not read this as "acquisitions that blocked".
    pub contended: u64,
    pub blocked_ns: u64,
}

impl LockSample {
    /// This sample minus an earlier one, for scoping to a hold.
    ///
    /// `saturating_sub` on both: a counter cannot go backwards, so a negative
    /// difference would mean the baseline came from a different cell, and
    /// clamping is the reading that cannot invent contention.
    pub fn since(&self, base: &LockSample) -> LockSample {
        LockSample {
            contended: self.contended.saturating_sub(base.contended),
            blocked_ns: self.blocked_ns.saturating_sub(base.blocked_ns),
        }
    }
}

/// A `parking_lot::Mutex` that counts what it cost to wait for.
///
/// Two ways in. [`with_locked`](Self::with_locked) hands the critical section
/// to the lock and is measured from outside it; [`lock`](Self::lock) is the
/// drop-in and pays a clock read and an atomic inside the hold. Prefer the
/// first wherever the caller's critical section is one expression.
#[derive(Debug, Default)]
pub struct CountedMutex<T> {
    inner: Mutex<T>,
    stats: LockStats,
}

impl<T> CountedMutex<T> {
    pub fn new(v: T) -> CountedMutex<T> {
        CountedMutex { inner: Mutex::new(v), stats: LockStats::default() }
    }

    /// The guard, and what waiting for it cost — `None` if nothing did.
    ///
    /// Records nothing: the caller owes the counters a `record` once the guard
    /// is gone. Private, because that debt is easy to forget and the only
    /// callers are the two below.
    #[inline]
    fn lock_timed(&self) -> (MutexGuard<'_, T>, Option<u64>) {
        // The fast path: one CAS, no clock, no atomics. Identical to what a
        // bare `lock()` would have done.
        if let Some(g) = self.inner.try_lock() {
            return (g, None);
        }
        let t = std::time::Instant::now();
        let g = self.inner.lock();
        (g, Some(t.elapsed().as_nanos() as u64))
    }

    /// A whole critical section, measured without being lengthened by the
    /// measurement.
    ///
    /// **The ordering rests on ownership, not on the shape of this function.**
    /// The guard is *moved* into `run_and_release`, the associated function
    /// below, which owns it and drops it on return. That closes all three ways a
    /// `record` could end up inside the hold: before the call is caught by
    /// `with_locked_leaves_the_counters_alone_inside_the_critical_section`;
    /// inside the helper is impossible, because the helper has no `&self` and
    /// so has nothing to record through; and after the call the guard has been
    /// moved out of scope, so reaching for it is `E0382: borrow of moved
    /// value` rather than a silent regression. An earlier version of this
    /// leaned on the guard being a local of an inner block, which is a
    /// convention a later edit could quietly break — this does not.
    ///
    /// What is left inside the hold is the one `clock_gettime` that has to be
    /// there: a wait is not a number until it is over.
    #[inline]
    pub fn with_locked<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let (g, waited) = self.lock_timed();
        let r = Self::run_and_release(g, f);
        if let Some(ns) = waited {
            self.stats.record(ns);
        }
        r
    }

    /// The caller's critical section, run against a guard this function owns.
    ///
    /// Deliberately an associated function and not a method: with no `&self`
    /// there is no `stats` in scope, so the counters cannot be touched from
    /// the one place in this module where the lock is provably held. It takes
    /// the guard by value so that the guard is gone — not merely out of an
    /// inner block — by the time it returns.
    #[inline]
    fn run_and_release<R>(mut g: MutexGuard<'_, T>, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut g)
    }

    /// Acquires the mutex and records contention for this lock.
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.lock_bare()
    }

    /// The instrumented lock that still hands back a real
    /// `parking_lot::MutexGuard`.
    ///
    /// **It exists for one caller that does not use it yet.**
    /// `parking_lot::Condvar::wait` takes a `parking_lot::MutexGuard` and
    /// nothing else, and `WorkQueue`'s lanes park on exactly that — so as long
    /// as [`lock`](Self::lock) is the only entry point, `lock` can never
    /// become a wrapper and the residual can never be removed from the other
    /// callers. Splitting the two here means the `cell.rs` side of that change
    /// is one word at one call site rather than a redesign.
    ///
    /// "Bare" describes the *guard*, not the accounting: this is fully
    /// instrumented, and it has to be, because `WorkQueue::inner` is one of
    /// the two locks the whole exercise is about. What it cannot do is defer
    /// the wait, because a bare guard has nowhere to carry it — which is the
    /// entire residual, now confined to whoever needs the bare guard.
    #[inline]
    pub fn lock_bare(&self) -> MutexGuard<'_, T> {
        if let Some(g) = self.inner.try_lock() {
            return g;
        }
        // Counted before blocking. The failed CAS above already proved this
        // acquisition contended, so the add costs a thread that is holding
        // nothing — and it excludes itself from the wait measured below, which
        // is the right side to err on: the instrument's own cost is not the
        // lock's.
        self.stats.collision();
        let t = std::time::Instant::now();
        let g = self.inner.lock();
        self.stats.add_wait(t.elapsed().as_nanos() as u64);
        g
    }

    pub fn stats(&self) -> &LockStats {
        &self.stats
    }
}

/// The same, for a reader-writer lock.
///
/// Reads and writes share one counter pair on purpose. The question is how
/// much time the execution threads lost to this lock, and a reader blocked
/// behind a writer lost it just as surely as a writer blocked behind readers.
#[derive(Debug, Default)]
pub struct CountedRwLock<T> {
    inner: RwLock<T>,
    stats: LockStats,
}

impl<T> CountedRwLock<T> {
    pub fn new(v: T) -> CountedRwLock<T> {
        CountedRwLock { inner: RwLock::new(v), stats: LockStats::default() }
    }

    /// A read guard that has already paid for itself.
    ///
    /// The collision is counted before blocking, as in [`CountedMutex::lock`];
    /// the *wait* is carried by the guard and added once the guard releases.
    /// So unlike the mutex drop-in, nothing at all is written to the counters
    /// while this lock is held. See `RecordOnRelease`.
    ///
    /// Be honest about the size of that on *this* path: a read hold is shared,
    /// so an atomic taken inside one delays no other reader, only a writer
    /// queued behind it — and `Cell::modules`, the one `CountedRwLock` in the
    /// cell, is written on module install and read on every dispatch. The
    /// feedback loop the module header describes barely applies here. It is
    /// done anyway because the cost is one predicted branch, because the
    /// instrument should not have a path where it perturbs what it measures
    /// when it does not have to, and because doing it here is what shows the
    /// mechanism works for the path where it *would* matter.
    #[inline]
    pub fn read(&self) -> CountedReadGuard<'_, T> {
        if let Some(g) = self.inner.try_read() {
            return CountedReadGuard { inner: g, record: self.pending(0) };
        }
        self.stats.collision();
        let t = std::time::Instant::now();
        let g = self.inner.read();
        CountedReadGuard { inner: g, record: self.pending(t.elapsed().as_nanos() as u64) }
    }

    /// The same for a writer. Worth having even though writers are rare: a
    /// write hold is exclusive, so an atomic taken inside it is the one that
    /// would delay every reader queued behind it.
    #[inline]
    pub fn write(&self) -> CountedWriteGuard<'_, T> {
        if let Some(g) = self.inner.try_write() {
            return CountedWriteGuard { inner: g, record: self.pending(0) };
        }
        self.stats.collision();
        let t = std::time::Instant::now();
        let g = self.inner.write();
        CountedWriteGuard { inner: g, record: self.pending(t.elapsed().as_nanos() as u64) }
    }

    #[inline]
    fn pending(&self, waited_ns: u64) -> RecordOnRelease<'_> {
        RecordOnRelease { stats: &self.stats, waited_ns }
    }

    pub fn stats(&self) -> &LockStats {
        &self.stats
    }
}

/// A wait that lands on the counters when it is dropped.
///
/// It exists to be the **second** field of a guard. A struct's fields are
/// dropped in declaration order, so a `RecordOnRelease` declared below a lock
/// guard runs its `fetch_add` after that guard has released — which is the
/// whole trick, and it needs no `unsafe`. The two obvious alternatives both
/// cost something this module will not pay: an `Option` that the guard's own
/// `Drop` could `take()` puts a branch and an `unwrap` on *every* deref, and
/// `ManuallyDrop` needs the `unsafe` that `lib.rs` denies crate-wide.
///
/// **The dependency on field order is real and no test here catches it.**
/// Swapping the two fields of [`CountedReadGuard`] silently moves the
/// `fetch_add` back inside the hold, and no thread can observe the difference
/// — the two positions are one lock release apart. What *is* tested, and
/// deterministically, is the part that matters most: that nothing is recorded
/// at acquisition, so the counters are untouched for the whole of the caller's
/// hold however long it runs. See
/// `a_read_guard_records_its_wait_only_once_it_is_released`.
///
/// **The airtight version was offered and declined, on review.** An `Option`
/// the guard's own `Drop` could `take()` would make the ordering impossible to
/// get wrong, at the price of a branch and an `unwrap` on every deref — and
/// those land on `Cell::modules`' read path, which is taken on every dispatch.
/// That is a hot-path cost on the very path this module exists to measure,
/// paid to defend against a future editor reordering two fields in a struct
/// whose doc says in bold not to. Slightly compromising the measurement in
/// order to protect the measurement is the wrong way round, so the risk stays
/// documented rather than guarded. Recorded here so the decision does not read
/// later as an oversight.
struct RecordOnRelease<'a> {
    stats: &'a LockStats,
    /// Zero on an uncontended acquisition, which records nothing at all.
    waited_ns: u64,
}

impl Drop for RecordOnRelease<'_> {
    #[inline]
    fn drop(&mut self) {
        // The uncontended path pays exactly this branch, predicted not-taken,
        // with the lock already released. That is the price of the whole
        // mechanism on the fast path.
        if self.waited_ns != 0 {
            self.stats.add_wait(self.waited_ns);
        }
    }
}

/// A `parking_lot::RwLockReadGuard` that settles its wait on the way out.
///
/// Field order is load-bearing: `inner` releases first, `record` fires second.
pub struct CountedReadGuard<'a, T> {
    inner: RwLockReadGuard<'a, T>,
    /// Never read, and that is the point: it is here for its `Drop`, which
    /// runs after `inner` above has released the lock.
    #[allow(dead_code)]
    record: RecordOnRelease<'a>,
}

impl<T> std::ops::Deref for CountedReadGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

/// A `parking_lot::RwLockWriteGuard` that settles its wait on the way out.
///
/// Field order is load-bearing: `inner` releases first, `record` fires second.
pub struct CountedWriteGuard<'a, T> {
    inner: RwLockWriteGuard<'a, T>,
    /// Never read, and that is the point: it is here for its `Drop`, which
    /// runs after `inner` above has released the lock.
    #[allow(dead_code)]
    record: RecordOnRelease<'a>,
}

impl<T> std::ops::Deref for CountedWriteGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for CountedWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An uncontended acquisition counts nothing, and a contended one counts
    /// itself and roughly what it waited.
    ///
    /// The point of the first half is that the fast path must stay free: if
    /// `contended` moved on every acquisition this would be an atomic on the
    /// hot path, which is the failure mode the whole design avoids.
    #[test]
    fn only_a_contended_acquisition_is_counted_and_it_carries_its_wait() {
        let m = CountedMutex::new(0u32);
        for _ in 0..1000 {
            *m.lock() += 1;
        }
        assert_eq!(m.stats().snapshot(), LockSample { contended: 0, blocked_ns: 0 });

        let (s, real) = one_contended_acquisition(|m| drop(m.lock()));
        assert_eq!(s.contended, 1, "exactly one acquisition found it held");
        assert_bracketed(s.blocked_ns, real);
    }

    /// One contended acquisition, performed by `take`, against a holder that
    /// keeps the lock for a known interval.
    ///
    /// Parameterised on the acquisition because `lock` and `lock_bare` share a
    /// body today and are about to stop sharing one. Coverage that only
    /// reaches `lock_bare` through `lock`'s delegation would disappear on the
    /// day that delegation goes, which is the day it matters most.
    fn one_contended_acquisition(
        take: impl FnOnce(&CountedMutex<u32>),
    ) -> (LockSample, std::time::Duration) {
        let m = std::sync::Arc::new(CountedMutex::new(0u32));
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let m2 = m.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            let _g = m2.lock();
            b2.wait();
            std::thread::sleep(std::time::Duration::from_millis(40));
        });
        held.wait();
        let waited = std::time::Instant::now();
        take(&m);
        let real = waited.elapsed();
        t.join().unwrap();
        (m.stats().snapshot(), real)
    }

    /// `lock_bare` counts what `lock` counts. It is not a passthrough.
    ///
    /// The caller it is waiting for is `WorkQueue`'s lanes, and
    /// `WorkQueue::inner` is one of the two locks this module was built to
    /// weigh. An uninstrumented `lock_bare` would silently zero the headline
    /// row on the day `cell.rs` adopts it — a regression that would look
    /// exactly like good news.
    #[test]
    fn lock_bare_is_instrumented_exactly_as_lock_is() {
        let m = CountedMutex::new(0u32);
        for _ in 0..1000 {
            *m.lock_bare() += 1;
        }
        assert_eq!(
            m.stats().snapshot(),
            LockSample { contended: 0, blocked_ns: 0 },
            "the fast path has to stay free on this entry point too"
        );

        let (s, real) = one_contended_acquisition(|m| drop(m.lock_bare()));
        assert_eq!(s.contended, 1, "exactly one acquisition found it held");
        assert_bracketed(s.blocked_ns, real);
    }

    /// A `lock_bare` guard is what a `parking_lot::Condvar` will wait on.
    ///
    /// This is the whole reason the method exists, and it is a compile-time
    /// property before it is a runtime one: `Condvar::wait` takes a
    /// `&mut parking_lot::MutexGuard` and will not accept a wrapper that
    /// merely derefs to `T`. Written as a test so the claim is checked rather
    /// than asserted in prose — and so that if `lock_bare`'s return type ever
    /// picks up a wrapper, this file stops compiling instead of `cell.rs`
    /// doing so.
    #[test]
    fn a_lock_bare_guard_is_what_a_condvar_can_wait_on() {
        let m = std::sync::Arc::new(CountedMutex::new(false));
        let cv = std::sync::Arc::new(parking_lot::Condvar::new());
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (m2, cv2, r2) = (m.clone(), cv.clone(), ready.clone());
        let t = std::thread::spawn(move || {
            r2.wait();
            // Blocks until the waiter below parks, which releases the mutex.
            let mut g = m2.lock_bare();
            *g = true;
            cv2.notify_one();
        });

        let mut g = m.lock_bare();
        // Signalled while still holding, so the setter cannot take the lock
        // before `wait` releases it: the parked branch is the one exercised.
        ready.wait();
        while !*g {
            cv.wait(&mut g);
        }
        drop(g);
        t.join().unwrap();
    }

    /// The recorded wait must bracket what the caller observed, from *both*
    /// sides.
    ///
    /// The upper bound alone cannot catch a unit error and never could:
    /// `blocked_ns` is by construction a sub-interval of `real`, so a bound at
    /// twice `real` is unreachable and changing `as_nanos` to `as_micros`, or
    /// even `as_millis`, left every test in this file green. Only the lower
    /// bound makes the unit falsifiable — a 1000x deflation lands three orders
    /// below half of `real`.
    ///
    /// Half, rather than something tighter: `blocked_ns` misses only the
    /// failed `try_lock` and a counter increment, so the true ratio is within
    /// a microsecond of 1.0, and the slack is there for a descheduled test
    /// thread on a loaded box, not for a plausible bug.
    fn assert_bracketed(blocked_ns: u64, real: std::time::Duration) {
        let real = real.as_nanos() as u64;
        assert!(
            blocked_ns >= real / 2,
            "recorded {blocked_ns} ns against an observed {real} ns: too small by \
             more than half, which is what a wrong time unit looks like"
        );
        assert!(
            blocked_ns <= real * 2 + 1_000_000,
            "recorded {blocked_ns} ns against an observed {real} ns: the wait was \
             measured as longer than the caller was blocked"
        );
    }

    /// Differencing scopes a counter to a hold, and cannot go negative.
    #[test]
    fn a_sample_differences_against_a_baseline_and_clamps() {
        let a = LockSample { contended: 10, blocked_ns: 500 };
        let b = LockSample { contended: 30, blocked_ns: 900 };
        assert_eq!(b.since(&a), LockSample { contended: 20, blocked_ns: 400 });
        // A baseline from somewhere else clamps rather than wrapping to
        // eighteen quintillion, which is what `u64` subtraction would do.
        assert_eq!(a.since(&b), LockSample { contended: 0, blocked_ns: 0 });
    }

    /// The rw variant counts a reader blocked behind a writer, in nanoseconds.
    #[test]
    fn a_reader_blocked_behind_a_writer_is_counted() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        assert_eq!(l.read().to_owned(), 0);
        assert_eq!(l.stats().snapshot().contended, 0, "an uncontended read is free");

        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            let _g = l2.write();
            b2.wait();
            std::thread::sleep(std::time::Duration::from_millis(30));
        });
        held.wait();
        let waited = std::time::Instant::now();
        drop(l.read());
        let real = waited.elapsed();
        t.join().unwrap();
        assert_eq!(l.stats().snapshot().contended, 1);
        assert_bracketed(l.stats().snapshot().blocked_ns, real);
    }

    /// And a writer blocked behind a reader — the third clock read in the
    /// module, which no test reached before and which a unit error could
    /// therefore have lived in indefinitely.
    #[test]
    fn a_writer_blocked_behind_a_reader_is_counted() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            let _g = l2.read();
            b2.wait();
            std::thread::sleep(std::time::Duration::from_millis(30));
        });
        held.wait();
        let waited = std::time::Instant::now();
        drop(l.write());
        let real = waited.elapsed();
        t.join().unwrap();
        assert_eq!(l.stats().snapshot().contended, 1);
        assert_bracketed(l.stats().snapshot().blocked_ns, real);
    }

    /// A read guard records its acquisition wait after release.
    /// The owning thread can inspect the counters before and after dropping the guard.
    #[test]
    fn a_read_guard_records_its_wait_only_once_it_is_released() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            let _g = l2.write();
            b2.wait();
            std::thread::sleep(std::time::Duration::from_millis(30));
        });
        held.wait();
        let waited = std::time::Instant::now();
        let g = l.read();
        let real = waited.elapsed();
        let inside = l.stats().snapshot();
        drop(g);
        let after = l.stats().snapshot();
        t.join().unwrap();

        assert_eq!(inside.contended, 1, "the collision is counted before blocking");
        assert_eq!(
            inside.blocked_ns, 0,
            "the wait was added to the counters while the read lock was still held"
        );
        assert_bracketed(after.blocked_ns, real);
    }

    /// The caller observes unchanged counters inside the critical section.
    /// The guard is released before its acquisition wait is recorded.
    #[test]
    fn with_locked_leaves_the_counters_alone_inside_the_critical_section() {
        let m = std::sync::Arc::new(CountedMutex::new(0u32));
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let m2 = m.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            m2.with_locked(|v| {
                b2.wait();
                std::thread::sleep(std::time::Duration::from_millis(40));
                *v += 1;
            });
        });
        held.wait();
        let waited = std::time::Instant::now();
        let inside = m.with_locked(|v| {
            *v += 1;
            m.stats().snapshot()
        });
        let real = waited.elapsed();
        t.join().unwrap();

        assert_eq!(
            inside,
            LockSample::default(),
            "the counters were written while the lock was still held"
        );
        let after = m.stats().snapshot();
        assert_eq!(after.contended, 1, "and then they were written");
        assert_bracketed(after.blocked_ns, real);
        assert_eq!(m.with_locked(|v| *v), 2);
    }

    /// The drop-in `lock()` counts its collision *before* it blocks.
    ///
    /// Observed from the thread that is holding the lock: it can read the
    /// counter while the waiter is still parked, and sees the collision
    /// already there. Moving `collision()` back below `self.inner.lock()`
    /// makes this zero.
    ///
    /// The 60 ms is the assumption — that a spawned thread reaches a failed
    /// `try_lock` inside it — and it is the same assumption every other timing
    /// test in this file makes.
    #[test]
    fn a_collision_is_counted_before_the_thread_parks() {
        for (what, s) in [
            ("lock", counters_while_a_thread_is_blocked(|m| drop(m.lock()))),
            ("lock_bare", counters_while_a_thread_is_blocked(|m| drop(m.lock_bare()))),
        ] {
            assert_eq!(
                s.contended, 1,
                "{what}: the waiter had not been counted while it was still blocked, so the \
                 count is landing inside the next holder's critical section"
            );
            assert_eq!(s.blocked_ns, 0, "{what}: a wait cannot be recorded before it has ended");
        }
    }

    /// What the counters read while a second thread is still blocked.
    ///
    /// Parameterised on the acquisition for the same reason
    /// `one_contended_acquisition` is: `lock` and `lock_bare` share a body
    /// today and step 3 divides them, so a check that reaches one of them
    /// through the other stops being a check on the day it matters.
    fn counters_while_a_thread_is_blocked(
        take: impl FnOnce(&CountedMutex<u32>) + Send + 'static,
    ) -> LockSample {
        let m = std::sync::Arc::new(CountedMutex::new(0u32));
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let m2 = m.clone();
        let s2 = started.clone();
        let g = m.lock();
        let t = std::thread::spawn(move || {
            s2.wait();
            take(&m2);
        });
        started.wait();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let while_waiting = m.stats().snapshot();
        drop(g);
        t.join().unwrap();
        while_waiting
    }

    /// The rwlock paths count their collision before blocking, too.
    ///
    /// Header item (1) claims this of all three acquisition paths, and only
    /// `CountedMutex` was ever checked: moving `collision()` below the
    /// blocking acquisition in `read` or `write` left every other test in this
    /// file green. That is the same shape as `write()` having had no timing
    /// test before D2 — a property asserted uniformly and verified on one path
    /// — so the whole family is covered here rather than the member that
    /// happened to be noticed.
    #[test]
    fn a_read_collision_is_counted_before_the_thread_parks() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let s2 = started.clone();
        let g = l.write();
        let t = std::thread::spawn(move || {
            s2.wait();
            drop(l2.read());
        });
        started.wait();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let while_waiting = l.stats().snapshot();
        drop(g);
        t.join().unwrap();

        assert_eq!(
            while_waiting.contended, 1,
            "the reader was not counted while it was still blocked, so the count is \
             landing inside the writer's critical section"
        );
        assert_eq!(while_waiting.blocked_ns, 0, "a wait cannot be recorded before it has ended");
    }

    /// And the writer blocked behind a reader, which is the other half.
    #[test]
    fn a_write_collision_is_counted_before_the_thread_parks() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        let started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let s2 = started.clone();
        let g = l.read();
        let t = std::thread::spawn(move || {
            s2.wait();
            drop(l2.write());
        });
        started.wait();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let while_waiting = l.stats().snapshot();
        drop(g);
        t.join().unwrap();

        assert_eq!(
            while_waiting.contended, 1,
            "the writer was not counted while it was still blocked, so the count is \
             landing inside the reader's critical section"
        );
        assert_eq!(while_waiting.blocked_ns, 0, "a wait cannot be recorded before it has ended");
    }

    /// The *write* guard defers its wait, and an uncontended write is free.
    ///
    /// Found by the same audit and not asked for: the read path had both of
    /// these properties checked and the write path had neither, though it uses
    /// a separate `RecordOnRelease` on a separate struct. "The read one
    /// passes" was never evidence about this one.
    #[test]
    fn a_write_guard_records_its_wait_only_once_it_is_released() {
        let l = std::sync::Arc::new(CountedRwLock::new(0u32));
        *l.write() += 1;
        assert_eq!(
            l.stats().snapshot(),
            LockSample::default(),
            "an uncontended write records nothing"
        );

        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let l2 = l.clone();
        let b2 = held.clone();
        let t = std::thread::spawn(move || {
            let _g = l2.read();
            b2.wait();
            std::thread::sleep(std::time::Duration::from_millis(30));
        });
        held.wait();
        let waited = std::time::Instant::now();
        let g = l.write();
        let real = waited.elapsed();
        let inside = l.stats().snapshot();
        drop(g);
        let after = l.stats().snapshot();
        t.join().unwrap();

        assert_eq!(inside.contended, 1, "the collision is counted before blocking");
        assert_eq!(
            inside.blocked_ns, 0,
            "the wait was added to the counters while the write lock was still held"
        );
        assert_bracketed(after.blocked_ns, real);
    }

    /// The two counters are a cacheline apart, and stay there.
    ///
    /// Without this the padding is a comment: `Padded` could lose its
    /// `repr(align)` and every test above would still pass, because nothing
    /// they measure depends on where the bytes are.
    #[test]
    fn the_counters_do_not_share_a_cacheline() {
        let s = LockStats::default();
        let c = std::ptr::addr_of!(s.contended.0) as usize;
        let b = std::ptr::addr_of!(s.blocked_ns.0) as usize;
        assert!(
            b.abs_diff(c) >= 128,
            "contended and blocked_ns are {} bytes apart: a waiter's count and \
             a holder's wait are on one line again",
            b.abs_diff(c)
        );
    }
}

/// Supplies a shard selector for this key.
/// A key type chooses whether its representation needs hashing before indexing.
pub trait ShardKey {
    fn shard_key(&self) -> u64;
}

impl ShardKey for u64 {
    fn shard_key(&self) -> u64 {
        *self
    }
}

impl ShardKey for crate::InstanceId {
    fn shard_key(&self) -> u64 {
        self.0
    }
}

/// The keyed map behind `Cell::active`, with the structure left open.
///
/// **Why a trait and not just the sharded map.** Sharding is a hypothesis:
/// 48.8% of the time the execution threads spent blocked was on `Cell::active`,
/// every one of its uses is a single-key operation, so splitting the lock
/// should remove that. "Should" is the word behind most of the claims on this
/// branch that turned out to be wrong. The queue strategies are selectable at
/// runtime so that three designs can be compared under load rather than argued
/// about, and this is the same device for the same reason — with the extra
/// force that, per the module header, this module *cannot itself* tell a
/// sharded map that fixed the problem from one that moved it into false
/// sharing. A throughput comparison can.
///
/// **The operations are exactly the ones `Cell::active` performs**: five
/// single-key ones. Nothing iterates the map, takes its length on a dispatch
/// path, or holds an invariant across two keys — which is what made sharding
/// legal in the first place, and is also what keeps this trait implementable
/// by a lock-free map.
///
/// **Reads hand a closure in rather than a reference out.** A `get` returning
/// `&V` would be returning something that borrows the map — for every
/// implementation here, a live lock guard — and handing a guard across this
/// boundary is how a caller ends up holding a shard across a `clone` it did
/// not know it was doing. The closure form also puts the whole critical
/// section inside the implementation, which is what lets it drop the guard
/// before it touches the counters.
pub trait ActiveMap<K, V> {
    fn contains_key(&self, k: &K) -> bool;
    fn insert(&self, k: K, v: V) -> Option<V>;
    fn remove(&self, k: &K) -> Option<V>;
    /// `f` applied to the value under `k`, or `None` if there is none. `f`
    /// runs with the lock held: keep it to what the old `get(&k).map(..)` did.
    fn with<R, F: FnOnce(&V) -> R>(&self, k: &K, f: F) -> Option<R>;
    /// The same, with a mutable value — the `get_mut` cases.
    fn with_mut<R, F: FnOnce(&mut V) -> R>(&self, k: &K, f: F) -> Option<R>;
    /// What this map cost the threads that waited for it, as one figure, so
    /// that it stays comparable with the unsharded locks beside it in the same
    /// table.
    fn lock_stats(&self) -> LockSample;
    /// Not on any dispatch path: an implementation may take every lock in turn.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A keyed map protected by one mutex.
#[derive(Debug)]
pub struct PlainMap<K, V> {
    inner: CountedMutex<HashMap<K, V>>,
}

// Written out rather than derived: a derived `Default` would demand `K:
// Default, V: Default` for a map that starts empty and needs neither.
impl<K: Hash + Eq, V> Default for PlainMap<K, V> {
    fn default() -> PlainMap<K, V> {
        PlainMap::new()
    }
}

impl<K: Hash + Eq, V> PlainMap<K, V> {
    pub fn new() -> PlainMap<K, V> {
        PlainMap { inner: CountedMutex::new(HashMap::new()) }
    }

    pub fn lock_stats(&self) -> LockSample {
        self.inner.stats().snapshot()
    }
}

impl<K: Hash + Eq, V> ActiveMap<K, V> for PlainMap<K, V> {
    fn contains_key(&self, k: &K) -> bool {
        self.inner.with_locked(|m| m.contains_key(k))
    }

    fn insert(&self, k: K, v: V) -> Option<V> {
        self.inner.with_locked(move |m| m.insert(k, v))
    }

    fn remove(&self, k: &K) -> Option<V> {
        self.inner.with_locked(|m| m.remove(k))
    }

    fn with<R, F: FnOnce(&V) -> R>(&self, k: &K, f: F) -> Option<R> {
        self.inner.with_locked(|m| m.get(k).map(f))
    }

    fn with_mut<R, F: FnOnce(&mut V) -> R>(&self, k: &K, f: F) -> Option<R> {
        self.inner.with_locked(|m| m.get_mut(k).map(f))
    }

    fn lock_stats(&self) -> LockSample {
        PlainMap::lock_stats(self)
    }

    fn len(&self) -> usize {
        self.inner.with_locked(|m| m.len())
    }
}

/// One shard: a mutex, its map, and a cacheline of its own.
///
/// The alignment is the point. `CountedMutex<HashMap<..>>` is well under a
/// line, and a `Box<[..]>` of them packs contiguously, so without it every
/// shard shares a line with a neighbour and two threads on two *different*
/// shards write the same line — which is the exact cost sharding was supposed
/// to remove, reintroduced by the layout, and (see the module header) in a
/// form this module's counters read as zero.
///
/// It is redundant today, and kept anyway: `LockStats` pads each of its
/// counters onto its own line, so a `CountedMutex` is already 128-aligned and
/// striking this attribute out changes no offset — which was confirmed by
/// striking it out. It is here so that the separation of the shards does not
/// silently depend on a decision made two types away.
/// `no_two_shards_share_a_cacheline` guards the property rather than the
/// attribute, and does go red when the counters lose their padding.
#[repr(align(128))]
struct Shard<K, V> {
    map: CountedMutex<HashMap<K, V>>,
}

/// Keyed storage with one lock per shard.
/// Single-key operations acquire only the selected shard.
pub struct ShardedMap<K, V> {
    shards: Box<[Shard<K, V>]>,
    mask: u64,
}

impl<K: Hash + Eq, V> ShardedMap<K, V> {
    /// Builds a sharded map, rounding the shard count up to a power of two.
    pub fn new(shards: usize) -> ShardedMap<K, V> {
        let n = shards.max(1).next_power_of_two();
        ShardedMap {
            shards: (0..n).map(|_| Shard { map: CountedMutex::new(HashMap::new()) }).collect(),
            mask: (n - 1) as u64,
        }
    }

    #[inline]
    fn index(&self, k: u64) -> usize {
        (k & self.mask) as usize
    }

    /// The lock guarding this key. The caller locks it and operates on the map
    /// inside, so a single-key operation touches exactly one shard.
    ///
    /// This is the raw form, and it is the *un-deferred* one: whatever the
    /// caller does with the guard, the counters are written from inside the
    /// hold by [`CountedMutex::lock`]. The [`ActiveMap`] operations, which are
    /// measured from outside, are built on it and are what callers use.
    ///
    /// **Private, and that is the point.** It was public for the eight —
    /// latterly ten — `Cell::active` sites that reached their shard directly;
    /// every one of them now goes through [`ActiveMap`], so nothing outside
    /// this file called it any more. Sealing it makes "every operation on this
    /// map is measured from outside the lock" true by visibility rather than
    /// by convention — the same trade as `with_locked` moving its guard by
    /// value rather than trusting the shape of a block. A caller that
    /// genuinely needs several operations under one acquisition should get a
    /// method on the trait that says so, not a lock handed out to be used
    /// however.
    #[inline]
    fn shard(&self, k: u64) -> &CountedMutex<HashMap<K, V>> {
        &self.shards[self.index(k)].map
    }

    /// Every shard's contention, summed.
    ///
    /// One figure rather than 256, because the question this answers is "what
    /// did this map cost" and the answer has to stay comparable with the
    /// unsharded locks beside it in the same table.
    pub fn lock_stats(&self) -> LockSample {
        self.shards.iter().fold(LockSample::default(), |acc, s| {
            let x = s.map.stats().snapshot();
            LockSample {
                contended: acc.contended + x.contended,
                blocked_ns: acc.blocked_ns + x.blocked_ns,
            }
        })
    }

    /// Entries across all shards. Not on any dispatch path — it takes every
    /// lock in turn — and used only where the unsharded map's `len()` was.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.map.with_locked(|m| m.len())).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K: ShardKey + Hash + Eq, V> ActiveMap<K, V> for ShardedMap<K, V> {
    fn contains_key(&self, k: &K) -> bool {
        self.shard(k.shard_key()).with_locked(|m| m.contains_key(k))
    }

    fn insert(&self, k: K, v: V) -> Option<V> {
        let s = self.shard(k.shard_key());
        s.with_locked(move |m| m.insert(k, v))
    }

    fn remove(&self, k: &K) -> Option<V> {
        self.shard(k.shard_key()).with_locked(|m| m.remove(k))
    }

    fn with<R, F: FnOnce(&V) -> R>(&self, k: &K, f: F) -> Option<R> {
        self.shard(k.shard_key()).with_locked(|m| m.get(k).map(f))
    }

    fn with_mut<R, F: FnOnce(&mut V) -> R>(&self, k: &K, f: F) -> Option<R> {
        self.shard(k.shard_key()).with_locked(|m| m.get_mut(k).map(f))
    }

    fn lock_stats(&self) -> LockSample {
        ShardedMap::lock_stats(self)
    }

    fn len(&self) -> usize {
        ShardedMap::len(self)
    }
}

/// Open-addressed storage with atomic primary-table key probes and per-entry value access.
/// Structural changes use key-selected stripe locks.
/// Overflow storage handles a full primary table.
pub struct LockFreeMap<K, V> {
    slots: Box<[LockFreeSlot<K, V>]>,
    mask: u64,
    stripes: Box<[Mutex<()>]>,
    overflow: Mutex<HashMap<K, V>>,
    overflow_len: AtomicU64,
    overflowed: AtomicU64,
}

/// A held entry: the slot's index, and its value under the entry's lock.
///
/// The index travels with the guard because `remove` needs both — it takes the
/// value and then has to write the tombstone into the very slot it took it
/// from, before releasing. Re-probing for the index instead would be a second
/// lookup for something already known, and one that reads a key word the
/// caller is in the middle of changing.
type LockFreeEntry<'a, K, V> = MutexGuard<'a, Option<(K, V)>>;

/// One table entry: a key word that can be read without locking, and a value
/// that cannot.
struct LockFreeSlot<K, V> {
    /// [`LF_EMPTY`], [`LF_TOMB`], or a live key's [`ShardKey::shard_key`].
    ///
    /// Published with `Release` *after* the value is in place, and moved to
    /// `LF_TOMB` before the entry lock is dropped, so a lookup that sees a key
    /// can lock the entry and be certain of finding the value that key names.
    key: AtomicU64,
    val: Mutex<Option<(K, V)>>,
}

/// A slot that has never held a key. A probe stops here: nothing beyond it on
/// this path can be the key being looked for.
const LF_EMPTY: u64 = 0;
/// A slot whose key was removed. A probe must **continue past it** — the key it
/// wants may have been claimed beyond it while this slot was still occupied —
/// and a claim may reuse it.
const LF_TOMB: u64 = u64::MAX;

/// Whether a key's word is one this table spends on saying "no key here".
///
/// **Two key values cannot live in the table, and pretending otherwise is a
/// silent wrong answer rather than an error.** A key of `0` stored in a slot
/// reads back as [`LF_EMPTY`]: the entry becomes invisible to every probe,
/// `len` undercounts it, and `remove` cannot find it — while the value sits
/// there holding a responder nobody will ever call. Caught by the shared
/// agreement test, which starts its key space at zero.
///
/// `InstanceId::alloc` counts up from 1 and would need the age of the universe
/// to reach `u64::MAX`, so `Cell::active` never takes this path. It exists
/// because a map that is correct only for the key space its current caller
/// happens to use is not correct, and the next caller does not read this file.
/// Such keys go to the same fallback a full table uses — which is slower, and
/// is the one place in this structure where being slower is the right answer.
#[inline]
fn lf_reserved(want: u64) -> bool {
    want == LF_EMPTY || want == LF_TOMB
}

/// Key-selected locks for structural table updates.
/// Updates acquire a stripe before entry access, so claims of the same key serialize.
const LF_STRIPES: usize = 64;

/// Mixes sequential keys before open-addressed table indexing.
#[inline]
fn lf_mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl<K: Hash + Eq + ShardKey, V> LockFreeMap<K, V> {
    /// `capacity` is rounded up to a power of two and then doubled: an
    /// open-addressed table's probe length is a function of its load factor
    /// rather than its size, and at 50% the expected probe is under two slots
    /// where at 90% it is ten.
    pub fn new(capacity: usize) -> LockFreeMap<K, V> {
        let n = capacity.max(64).next_power_of_two() * 2;
        LockFreeMap {
            slots: (0..n)
                .map(|_| LockFreeSlot { key: AtomicU64::new(LF_EMPTY), val: Mutex::new(None) })
                .collect(),
            mask: (n - 1) as u64,
            stripes: (0..LF_STRIPES).map(|_| Mutex::new(())).collect(),
            overflow: Mutex::new(HashMap::new()),
            overflow_len: AtomicU64::new(0),
            overflowed: AtomicU64::new(0),
        }
    }

    /// Entries that have had to take the shared fallback.
    ///
    /// Zero in a run whose table was sized correctly. Non-zero says this map
    /// spent part of the run behaving like [`PlainMap`], which is the one way a
    /// figure taken from it could mislead.
    pub fn overflowed(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }

    #[inline]
    fn home(&self, want: u64) -> usize {
        (lf_mix(want) & self.mask) as usize
    }

    #[inline]
    fn stripe(&self, want: u64) -> &Mutex<()> {
        &self.stripes[lf_mix(want) as usize % LF_STRIPES]
    }

    #[inline]
    fn next(&self, i: usize) -> usize {
        (i + 1) & (self.mask as usize)
    }

    /// The slot holding `k`, by atomic loads alone.
    ///
    /// `None` on reaching an [`LF_EMPTY`] slot, which is proof of absence: a
    /// claim never skips an empty slot, so a key in the table is always found
    /// before one.
    #[inline]
    fn find(&self, k: &K) -> Option<usize> {
        let want = k.shard_key();
        if lf_reserved(want) {
            // Never in the table by construction — see `lf_reserved`. Without
            // this, `LF_TOMB` would scan every slot to conclude the same thing.
            return None;
        }
        let mut i = self.home(want);
        for _ in 0..self.slots.len() {
            match self.slots[i].key.load(Ordering::Acquire) {
                LF_EMPTY => return None,
                x if x == want => return Some(i),
                _ => i = self.next(i),
            }
        }
        None
    }

    /// [`Self::find`], the entry's lock, and the re-check that makes the pair
    /// mean something.
    ///
    /// A slot can be removed and re-claimed by another key between the probe
    /// and the lock, so the key word is read again under the lock and the
    /// stored key compared: `shard_key` is not required to be injective, and a
    /// map that trusted it would hand back one key's value for another's.
    ///
    /// **The retry is on the key word changing, never on the value.** Once the
    /// word still reads `want` under the lock this returns an answer either
    /// way, so a caller cannot spin against a state that a writer is not
    /// actively leaving.
    fn locked(&self, k: &K) -> Option<(usize, LockFreeEntry<'_, K, V>)> {
        let want = k.shard_key();
        loop {
            let i = self.find(k)?;
            let g = self.slots[i].val.lock();
            if self.slots[i].key.load(Ordering::Acquire) != want {
                continue;
            }
            return g.as_ref().is_some_and(|(have, _)| have == k).then_some((i, g));
        }
    }

    /// Whether anything is in the fallback, as one load.
    ///
    /// **This is what keeps a lookup lock-free.** The fallback is a shared map
    /// behind a mutex, and consulting it on every miss would put that mutex
    /// back on the path this structure exists to take one off. In a correctly
    /// sized run it is empty throughout and every miss pays one atomic load.
    #[inline]
    fn overflow_occupied(&self) -> bool {
        self.overflow_len.load(Ordering::Acquire) != 0
    }

    fn publish_overflow_len(&self, m: &HashMap<K, V>) {
        self.overflow_len.store(m.len() as u64, Ordering::Release);
    }

    /// Place a key the table will not hold. Does not touch `overflowed`: the
    /// two callers count for different reasons and only one is about capacity.
    fn insert_overflow(&self, k: K, v: V) -> Option<V> {
        let mut m = self.overflow.lock();
        let old = m.insert(k, v);
        self.publish_overflow_len(&m);
        old
    }

    /// This backend does not collect lock counters.
    /// The returned default sample is not a measurement.
    pub fn lock_stats(&self) -> LockSample {
        LockSample::default()
    }

    /// Not on any dispatch path. Scans every key word.
    pub fn len(&self) -> usize {
        let in_table = self
            .slots
            .iter()
            .filter(|s| !matches!(s.key.load(Ordering::Acquire), LF_EMPTY | LF_TOMB))
            .count();
        in_table + self.overflow_len.load(Ordering::Acquire) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K: Hash + Eq + ShardKey, V> ActiveMap<K, V> for LockFreeMap<K, V> {
    /// Checks key presence at this instant.
    /// This read does not reserve the key or make a following insertion atomic with it.
    fn contains_key(&self, k: &K) -> bool {
        if self.find(k).is_some() {
            return true;
        }
        self.overflow_occupied() && self.overflow.lock().contains_key(k)
    }

    fn insert(&self, k: K, v: V) -> Option<V> {
        let want = k.shard_key();
        // Stripe first, entry second, everywhere. See `LF_STRIPES`.
        let _claim = self.stripe(want).lock();

        // A key the table cannot represent goes to the fallback and is **not**
        // counted as an overflow: `overflowed` answers "was this table too
        // small", and a reserved key says nothing about its size.
        if lf_reserved(want) {
            return self.insert_overflow(k, v);
        }

        if let Some((_, mut g)) = self.locked(&k) {
            return g.replace((k, v)).map(|(_, old)| old);
        }

        // An `Option` rather than the values themselves: the payload is placed
        // inside a loop, and the borrow checker is right that a value moved in
        // a loop body has to be gone by the next iteration.
        let mut payload = Some((k, v));
        let mut i = self.home(want);
        for _ in 0..self.slots.len() {
            if matches!(self.slots[i].key.load(Ordering::Acquire), LF_EMPTY | LF_TOMB) {
                let mut g = self.slots[i].val.lock();
                // Re-read under the entry lock: another key's claim, on a
                // different stripe, may have taken this slot since the load.
                if matches!(self.slots[i].key.load(Ordering::Acquire), LF_EMPTY | LF_TOMB) {
                    *g = payload.take();
                    // **Value first, key second.** A lookup that sees the key
                    // must find the value it names; publishing the key first
                    // would let one arrive at an entry holding `None`.
                    self.slots[i].key.store(want, Ordering::Release);
                    return None;
                }
            }
            i = self.next(i);
        }

        let (k, v) = payload.take().expect("the loop either places the payload or leaves it");
        self.overflowed.fetch_add(1, Ordering::Relaxed);
        self.insert_overflow(k, v)
    }

    fn remove(&self, k: &K) -> Option<V> {
        let want = k.shard_key();
        let _claim = self.stripe(want).lock();
        if let Some((i, mut g)) = self.locked(k) {
            let taken = g.take();
            // **Under the entry lock, before it is dropped.** A lookup that
            // held the key word valid while the value was already gone would
            // be the one state `locked` cannot tell from a live entry.
            self.slots[i].key.store(LF_TOMB, Ordering::Release);
            drop(g);
            return taken.map(|(_, v)| v);
        }
        if !self.overflow_occupied() {
            return None;
        }
        let mut m = self.overflow.lock();
        let old = m.remove(k);
        self.publish_overflow_len(&m);
        old
    }

    /// Runs the read closure within this key's scoped value access.
    fn with<R, F: FnOnce(&V) -> R>(&self, k: &K, f: F) -> Option<R> {
        match self.locked(k) {
            Some((_, g)) => g.as_ref().map(|(_, v)| f(v)),
            None if self.overflow_occupied() => self.overflow.lock().get(k).map(f),
            None => None,
        }
    }

    fn with_mut<R, F: FnOnce(&mut V) -> R>(&self, k: &K, f: F) -> Option<R> {
        match self.locked(k) {
            Some((_, mut g)) => g.as_mut().map(|(_, v)| f(v)),
            None if self.overflow_occupied() => self.overflow.lock().get_mut(k).map(f),
            None => None,
        }
    }

    fn lock_stats(&self) -> LockSample {
        LockFreeMap::lock_stats(self)
    }

    fn len(&self) -> usize {
        LockFreeMap::len(self)
    }
}

/// Selects the storage backend for the active-instance index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ActiveMapKind {
    /// One mutex over one `HashMap` — [`PlainMap`].
    Plain,
    /// One mutex per shard, indexed by the key's low bits — [`ShardedMap`].
    ///
    /// What this branch ran before [`ActiveMapKind::LockFree`] existed, and
    /// the thing it has to beat. One flag away.
    Sharded,
    /// An open-addressed table with lock-free lookup — [`LockFreeMap`].
    ///
    /// **The default, which is a claim and should be read as one.** Sharding
    /// split the lock that carried 48.8% of the execution pool's blocked time;
    /// this removes it from the lookups altogether and narrows it to one entry
    /// for the rest. Whether that is a win end to end is a throughput
    /// question, and the other two arms are here to answer it — the module
    /// header's warning about contention that turns into false sharing reads
    /// on this arm exactly as it does on `Sharded`.
    #[default]
    LockFree,
}

/// The runtime selection, as one concrete type a struct can hold.
///
/// An enum rather than `Box<dyn ActiveMap>`: the trait's read operations are
/// generic over the caller's closure and so cannot go through a vtable, and a
/// map that is touched six times per dispatch should not acquire an indirect
/// call per touch in order to be swappable.
///
/// It is 512 bytes — [`PlainMap`]'s 384, half of which is its two padded
/// counters, rounded up to the 128-byte alignment those counters force — and
/// `clippy::large_enum_variant` would rather the large arm were boxed. One of
/// these exists per cell, inside an `Arc<Cell>`, never moved and never in a
/// collection, so the lint's cost model does not apply — while its remedy
/// would put a pointer hop on every operation of the baseline, biasing the one
/// comparison this enum exists to make.
#[allow(clippy::large_enum_variant)]
pub enum ActiveMapImpl<K, V> {
    Plain(PlainMap<K, V>),
    Sharded(ShardedMap<K, V>),
    LockFree(LockFreeMap<K, V>),
}

impl<K: Hash + Eq + ShardKey, V> ActiveMapImpl<K, V> {
    /// `shards` sizes [`ActiveMapKind::Sharded`] and `capacity` sizes
    /// [`ActiveMapKind::LockFree`]; [`ActiveMapKind::Plain`] has one lock and
    /// one map and ignores both.
    ///
    /// Two numbers rather than one because they are not the same quantity: a
    /// shard count is chosen against the *thread* count, to make collisions
    /// unlikely, and a table capacity against the *entry* count, to keep the
    /// load factor low. Passing one for the other silently gives a table that
    /// fills.
    pub fn new(kind: ActiveMapKind, shards: usize, capacity: usize) -> ActiveMapImpl<K, V> {
        match kind {
            ActiveMapKind::Plain => ActiveMapImpl::Plain(PlainMap::new()),
            ActiveMapKind::Sharded => ActiveMapImpl::Sharded(ShardedMap::new(shards)),
            ActiveMapKind::LockFree => ActiveMapImpl::LockFree(LockFreeMap::new(capacity)),
        }
    }

    pub fn kind(&self) -> ActiveMapKind {
        match self {
            ActiveMapImpl::Plain(_) => ActiveMapKind::Plain,
            ActiveMapImpl::Sharded(_) => ActiveMapKind::Sharded,
            ActiveMapImpl::LockFree(_) => ActiveMapKind::LockFree,
        }
    }

    /// Entries that took [`LockFreeMap`]'s shared fallback. `0` under the two
    /// structures that have none.
    pub fn overflowed(&self) -> u64 {
        match self {
            ActiveMapImpl::LockFree(m) => m.overflowed(),
            _ => 0,
        }
    }
}

impl<K: ShardKey + Hash + Eq, V> ActiveMap<K, V> for ActiveMapImpl<K, V> {
    fn contains_key(&self, k: &K) -> bool {
        match self {
            ActiveMapImpl::Plain(m) => m.contains_key(k),
            ActiveMapImpl::Sharded(m) => m.contains_key(k),
            ActiveMapImpl::LockFree(m) => m.contains_key(k),
        }
    }

    fn insert(&self, k: K, v: V) -> Option<V> {
        match self {
            ActiveMapImpl::Plain(m) => m.insert(k, v),
            ActiveMapImpl::Sharded(m) => m.insert(k, v),
            ActiveMapImpl::LockFree(m) => m.insert(k, v),
        }
    }

    fn remove(&self, k: &K) -> Option<V> {
        match self {
            ActiveMapImpl::Plain(m) => m.remove(k),
            ActiveMapImpl::Sharded(m) => m.remove(k),
            ActiveMapImpl::LockFree(m) => m.remove(k),
        }
    }

    fn with<R, F: FnOnce(&V) -> R>(&self, k: &K, f: F) -> Option<R> {
        match self {
            ActiveMapImpl::Plain(m) => m.with(k, f),
            ActiveMapImpl::Sharded(m) => m.with(k, f),
            ActiveMapImpl::LockFree(m) => m.with(k, f),
        }
    }

    fn with_mut<R, F: FnOnce(&mut V) -> R>(&self, k: &K, f: F) -> Option<R> {
        match self {
            ActiveMapImpl::Plain(m) => m.with_mut(k, f),
            ActiveMapImpl::Sharded(m) => m.with_mut(k, f),
            ActiveMapImpl::LockFree(m) => m.with_mut(k, f),
        }
    }

    fn lock_stats(&self) -> LockSample {
        match self {
            ActiveMapImpl::Plain(m) => PlainMap::lock_stats(m),
            ActiveMapImpl::Sharded(m) => ShardedMap::lock_stats(m),
            ActiveMapImpl::LockFree(m) => LockFreeMap::lock_stats(m),
        }
    }

    fn len(&self) -> usize {
        match self {
            ActiveMapImpl::Plain(m) => ActiveMap::len(m),
            ActiveMapImpl::Sharded(m) => ShardedMap::len(m),
            ActiveMapImpl::LockFree(m) => LockFreeMap::len(m),
        }
    }
}

#[cfg(test)]
mod sharded_tests {
    use super::*;

    /// Sequential keys land on distinct shards, which is the whole point of
    /// indexing on the low bits for a counter-derived id.
    #[test]
    fn sequential_keys_spread_perfectly_and_a_key_always_finds_its_own_shard() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(64);
        for k in 0..64u64 {
            m.insert(k, k as u32);
        }
        // Every shard got exactly one, so 64 threads on 64 sequential ids
        // never collide.
        assert_eq!(ShardedMap::len(&m), 64);
        for s in m.shards.iter() {
            assert_eq!(s.map.with_locked(|x| x.len()), 1, "a counter's low bits are a round-robin");
        }
        // And a key routes to the same shard every time, or `remove` would
        // miss what `insert` wrote.
        for k in 0..64u64 {
            assert_eq!(m.with(&k, |v| *v), Some(k as u32));
            assert_eq!(m.remove(&k), Some(k as u32));
        }
        assert!(m.is_empty());
    }

    /// A non-power-of-two request rounds up, so the mask is always valid.
    #[test]
    fn a_shard_count_rounds_up_to_a_power_of_two() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(100);
        assert_eq!(m.shards.len(), 128);
        assert_eq!(m.mask, 127);
        let m: ShardedMap<u64, u32> = ShardedMap::new(0);
        assert_eq!(m.shards.len(), 1, "zero shards is one shard, not a divide by zero");
    }

    /// No two shards share a cacheline.
    ///
    /// Checked on the real array rather than on `size_of`, because what the
    /// hardware sees is the stride and the base: a 128-byte type laid out at
    /// 64-byte offsets would satisfy any assertion about its size.
    #[test]
    fn no_two_shards_share_a_cacheline() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(8);
        let addr = |i: usize| std::ptr::addr_of!(m.shards[i]) as usize;
        assert_eq!(addr(0) % 128, 0, "the shard array does not start on a line boundary");
        for i in 1..8 {
            assert!(
                addr(i) - addr(i - 1) >= 128,
                "shards {} and {} are {} bytes apart, so two threads on two \
                 different shards write one line",
                i - 1,
                i,
                addr(i) - addr(i - 1)
            );
        }
        // The number the doc quotes, kept honest. A change in `HashMap`'s
        // layout may move it; a change that makes it smaller than a line is a
        // bug.
        assert_eq!(std::mem::size_of::<Shard<u64, u32>>(), 384);
    }

    /// Both implementations answer a sequence of single-key operations
    /// identically — which is what "selectable at runtime" has to mean.
    #[test]
    fn the_two_implementations_agree_on_every_operation() {
        fn exercise<M: ActiveMap<u64, u32>>(m: &M, what: &str) {
            assert!(!m.contains_key(&7), "{what}: empty");
            assert_eq!(m.insert(7, 70), None, "{what}: first insert returns nothing");
            assert_eq!(m.insert(7, 71), Some(70), "{what}: an overwrite returns the old value");
            assert!(m.contains_key(&7), "{what}: present");
            assert_eq!(m.with(&7, |v| *v), Some(71), "{what}: read back");
            assert_eq!(m.with(&8, |v| *v), None, "{what}: a missing key reads as None");
            assert_eq!(
                m.with_mut(&7, |v| {
                    *v += 1;
                    *v
                }),
                Some(72),
                "{what}: mutate in place"
            );
            assert_eq!(m.with_mut(&8, |v| *v), None, "{what}: and a missing one is not created");
            m.insert(8, 80);
            m.insert(1 << 40, 99);
            assert_eq!(m.len(), 3, "{what}: len");
            assert!(!m.is_empty(), "{what}: not empty");
            assert_eq!(m.remove(&7), Some(72), "{what}: remove returns the value");
            assert_eq!(m.remove(&7), None, "{what}: and only once");
            assert_eq!(m.remove(&8), Some(80));
            assert_eq!(m.remove(&(1 << 40)), Some(99), "{what}: a key above the mask still routes");
            assert!(m.is_empty(), "{what}: empty again");
        }
        exercise(&PlainMap::<u64, u32>::new(), "plain");
        exercise(&ShardedMap::<u64, u32>::new(16), "sharded");
        exercise(&LockFreeMap::<u64, u32>::new(64), "lock-free");
        // **And once at a capacity too small to hold the keys**, which is the
        // only way the shared fallback is ever reached and therefore the only
        // way it is ever tested. `new` floors the table at 64 entries, so the
        // overflow is exercised by the dedicated test below rather than here;
        // this pins that a small table still answers identically.
        exercise(&LockFreeMap::<u64, u32>::new(1), "lock-free/small");
        exercise(&ActiveMapImpl::<u64, u32>::new(ActiveMapKind::Plain, 16, 64), "impl/plain");
        exercise(&ActiveMapImpl::<u64, u32>::new(ActiveMapKind::Sharded, 16, 64), "impl/sharded");
        exercise(&ActiveMapImpl::<u64, u32>::new(ActiveMapKind::LockFree, 16, 64), "impl/lock-free");
    }

    /// **The overflow is reachable, correct, and counted.**
    ///
    /// It is the arm of `LockFreeMap` that nothing exercises in a
    /// correctly-sized run, which is exactly why it needs a test: a fallback
    /// that only runs when the table is full is a fallback that first runs in
    /// production, on the largest population anyone has tried.
    #[test]
    fn a_full_lock_free_table_falls_back_without_losing_a_key() {
        // 128 slots, and deliberately more keys than that.
        let m = LockFreeMap::<u64, u32>::new(1);
        let n = 400u64;
        for k in 0..n {
            assert_eq!(m.insert(k, k as u32), None, "key {k} was not already present");
        }
        assert!(m.overflowed() > 0, "400 keys into a 128-slot table must have overflowed");
        assert_eq!(m.len() as u64, n, "every key is somewhere");

        // Every key reads back its own value, whichever side of the fallback
        // it landed on — the failure this guards against is a lookup that
        // finds the table empty of a key and stops, never consulting the map
        // it was put in.
        for k in 0..n {
            assert!(m.contains_key(&k), "key {k} is not findable");
            assert_eq!(m.with(&k, |v| *v), Some(k as u32), "key {k} reads back wrong");
        }
        for k in 0..n {
            assert_eq!(m.remove(&k), Some(k as u32), "key {k} did not come back out");
        }
        assert!(m.is_empty(), "left over: {}", m.len());
    }

    /// **The two key values the table spends on its own bookkeeping.**
    ///
    /// A key of `0` written into a slot reads back as an empty slot: the entry
    /// goes invisible to every probe, `len` undercounts it, and `remove`
    /// cannot find it — while the value sits there holding a responder nobody
    /// will call. That is a hung request, surfacing far from its cause.
    ///
    /// It was a real bug in this map, and three unrelated tests failed on it
    /// at once because all three happened to start counting at zero. A caller
    /// whose key space starts at one — which `Cell::active`'s does — would not
    /// have found it at all.
    #[test]
    fn the_sentinel_key_values_are_stored_and_read_like_any_other() {
        for k in [0u64, u64::MAX] {
            let m = LockFreeMap::<u64, u32>::new(64);
            assert!(!m.contains_key(&k), "{k}: empty to begin with");
            assert_eq!(m.insert(k, 7), None, "{k}: first insert");
            assert!(m.contains_key(&k), "{k}: a reserved key must still be findable");
            assert_eq!(m.with(&k, |v| *v), Some(7), "{k}: reads back");
            assert_eq!(m.with_mut(&k, |v| { *v += 1; *v }), Some(8), "{k}: mutates");
            assert_eq!(m.len(), 1, "{k}: counted");
            assert_eq!(m.insert(k, 9), Some(8), "{k}: overwrite returns the old value");
            assert_eq!(m.remove(&k), Some(9), "{k}: comes back out");
            assert!(m.is_empty(), "{k}: and is gone");

            // It takes the fallback, but it is **not** an overflow: that
            // counter answers "was this table too small", and a reserved key
            // says nothing about its size. Conflating them would make every
            // run using such a key look like one that had outgrown its table.
            assert_eq!(m.overflowed(), 0, "{k}: a reserved key is not a capacity failure");
        }
    }

    /// **Tombstones do not hide a key that was inserted past them.**
    ///
    /// The bug this exists for is the one an open-addressed table gets wrong
    /// by default: a `remove` that writes EMPTY rather than a tombstone
    /// truncates every probe path running through that slot, and the keys
    /// beyond it become invisible while still occupying the table. It shows up
    /// as a request that never completes, long after the removal that caused
    /// it, and only when two keys happened to collide.
    #[test]
    fn a_removal_does_not_hide_the_keys_probed_past_it() {
        let m = LockFreeMap::<u64, u32>::new(64);
        // Enough keys that some share a home slot, then churn them: every
        // removal leaves a tombstone that later probes must run through.
        for k in 0..200u64 {
            m.insert(k, k as u32);
        }
        for k in (0..200u64).step_by(2) {
            assert_eq!(m.remove(&k), Some(k as u32));
        }
        for k in (1..200u64).step_by(2) {
            assert_eq!(m.with(&k, |v| *v), Some(k as u32), "key {k} was hidden by a tombstone");
        }
        // And the freed slots are reusable rather than lost.
        for k in 200..300u64 {
            assert_eq!(m.insert(k, k as u32), None);
        }
        for k in 200..300u64 {
            assert_eq!(m.with(&k, |v| *v), Some(k as u32));
        }
    }

    /// **Sixty-four threads, disjoint keys, the whole operation set.**
    ///
    /// The reason a structure like this gets a stress test rather than only
    /// unit cases: every hazard in it is a window between two instructions —
    /// a slot claimed between a probe and a lock, a key published before its
    /// value, a tombstone written before a reader has finished with the entry
    /// — and none of them is reachable by a single thread. This will not prove
    /// the map correct, but each of those windows is one it can fall into.
    #[test]
    fn concurrent_keys_do_not_cross() {
        use std::sync::Arc;
        let m = Arc::new(LockFreeMap::<u64, u64>::new(1024));
        let threads = 64u64;
        let per = 200u64;
        let start = Arc::new(std::sync::Barrier::new(threads as usize));

        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let m = m.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for i in 0..per {
                        // Disjoint by construction: a key belongs to exactly
                        // one thread, so any value another thread's key could
                        // return is a crossed entry rather than a race the
                        // caller was asking for.
                        let k = t * per + i;
                        assert_eq!(m.insert(k, k * 3), None, "t{t} k{k} was already present");
                        assert!(m.contains_key(&k));
                        assert_eq!(m.with(&k, |v| *v), Some(k * 3), "t{t} k{k} read back wrong");
                        assert_eq!(m.with_mut(&k, |v| { *v += 1; *v }), Some(k * 3 + 1));
                        assert_eq!(m.remove(&k), Some(k * 3 + 1), "t{t} k{k} came back wrong");
                        assert!(!m.contains_key(&k), "t{t} k{k} outlived its removal");
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().expect("no thread panicked");
        }
        assert!(m.is_empty(), "{} entries outlived the run", m.len());
        assert_eq!(m.overflowed(), 0, "a 2048-slot table must hold 64 live keys without spilling");
    }

    /// A collision through the map API reaches the map's own figures.
    ///
    /// The path that records has to be walked by a test or the whole module
    /// reports zero and looks like a lock nobody waits for: `with_locked`
    /// records after releasing, and "after releasing" is exactly where a
    /// forgotten call goes unnoticed.
    #[test]
    fn contention_inside_a_shard_reaches_the_map_figures() {
        // **The two counted structures only.** `LockFree` reports no sample at
        // all and `Cell` omits its row — see `LockFreeMap::lock_stats` — so
        // asserting a figure from it here would be asserting the zero this
        // module exists to keep out of tables.
        for (kind, name) in [(ActiveMapKind::Plain, "plain"), (ActiveMapKind::Sharded, "sharded")] {
            let m = std::sync::Arc::new(ActiveMapImpl::<u64, u32>::new(kind, 16, 64));
            m.insert(3, 0);
            assert_eq!(m.lock_stats(), LockSample::default(), "{name}: uncontended is free");

            let held = std::sync::Arc::new(std::sync::Barrier::new(2));
            let m2 = m.clone();
            let b2 = held.clone();
            let t = std::thread::spawn(move || {
                m2.with_mut(&3, |v| {
                    b2.wait();
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    *v += 1;
                });
            });
            held.wait();
            let waited = std::time::Instant::now();
            m.with_mut(&3, |v| *v += 1);
            let real = waited.elapsed().as_nanos() as u64;
            t.join().unwrap();

            let s = m.lock_stats();
            assert_eq!(s.contended, 1, "{name}: the contended acquisition was not counted");
            assert!(
                s.blocked_ns >= real / 2 && s.blocked_ns <= real * 2 + 1_000_000,
                "{name}: recorded {} ns against an observed {real} ns",
                s.blocked_ns
            );
            assert_eq!(m.with(&3, |v| *v), Some(2), "{name}: and both writes landed");
        }
    }
}
