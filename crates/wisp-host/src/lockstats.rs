//! Contended lock-acquisition counts and elapsed wait time.
//! Shared counters are updated only after a failed immediate acquisition.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Contended acquisitions of one lock, and the time they cost.
#[derive(Debug, Default)]
pub struct LockStats {
    /// Acquisitions that found the lock already held.
    ///
    /// Deliberately not a count of *all* acquisitions: an uncontended one is
    /// free and uninteresting, and counting it would put a shared atomic on
    /// the fast path, which is the one thing this must not do.
    pub contended: AtomicU64,
    /// Nanoseconds spent blocked, summed across threads.
    ///
    /// The figure to rank on. A lock taken often but never held long costs
    /// less than one taken rarely and held across a syscall, and a contended
    /// *count* cannot tell those apart.
    pub blocked_ns: AtomicU64,
}

impl LockStats {
    fn record(&self, ns: u64) {
        self.contended.fetch_add(1, Ordering::Relaxed);
        self.blocked_ns.fetch_add(ns, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LockSample {
        LockSample {
            contended: self.contended.load(Ordering::Relaxed),
            blocked_ns: self.blocked_ns.load(Ordering::Relaxed),
        }
    }
}

/// One lock's figures at an instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockSample {
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

/// A parking_lot mutex with contention counts and acquisition-wait measurements.
#[derive(Debug, Default)]
pub struct CountedMutex<T> {
    inner: Mutex<T>,
    stats: LockStats,
}

impl<T> CountedMutex<T> {
    pub fn new(v: T) -> CountedMutex<T> {
        CountedMutex { inner: Mutex::new(v), stats: LockStats::default() }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        // The fast path: one CAS, no clock, no atomics. Identical to what a
        // bare `lock()` would have done.
        if let Some(g) = self.inner.try_lock() {
            return g;
        }
        // The slow path was already going to park on a futex.
        let t = std::time::Instant::now();
        let g = self.inner.lock();
        self.stats.record(t.elapsed().as_nanos() as u64);
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

    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        if let Some(g) = self.inner.try_read() {
            return g;
        }
        let t = std::time::Instant::now();
        let g = self.inner.read();
        self.stats.record(t.elapsed().as_nanos() as u64);
        g
    }

    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        if let Some(g) = self.inner.try_write() {
            return g;
        }
        let t = std::time::Instant::now();
        let g = self.inner.write();
        self.stats.record(t.elapsed().as_nanos() as u64);
        g
    }

    pub fn stats(&self) -> &LockStats {
        &self.stats
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

        // Hold it from another thread for a known interval, then take it.
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
        drop(m.lock());
        let real = waited.elapsed();
        t.join().unwrap();

        let s = m.stats().snapshot();
        assert_eq!(s.contended, 1, "exactly one acquisition found it held");
        assert!(s.blocked_ns > 0, "and it must carry a wait, got {}", s.blocked_ns);
        // Within an order of magnitude of what the caller actually observed —
        // loose, because this is a scheduler on a shared machine, but tight
        // enough to catch a wrong unit or a stopped clock.
        assert!(
            s.blocked_ns <= real.as_nanos() as u64 * 2 + 1_000_000,
            "recorded {} ns against an observed {} ns",
            s.blocked_ns,
            real.as_nanos()
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

    /// The rw variant counts a reader blocked behind a writer.
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
        drop(l.read());
        t.join().unwrap();
        assert_eq!(l.stats().snapshot().contended, 1);
        assert!(l.stats().snapshot().blocked_ns > 0);
    }
}

/// Keyed storage with one lock per shard.
/// Single-key operations acquire only the selected shard.
pub struct ShardedMap<K, V> {
    shards: Box<[CountedMutex<std::collections::HashMap<K, V>>]>,
    mask: u64,
}

impl<K: std::hash::Hash + Eq, V> ShardedMap<K, V> {
    /// Builds a sharded map, rounding the shard count up to a power of two.
    pub fn new(shards: usize) -> ShardedMap<K, V> {
        let n = shards.max(1).next_power_of_two();
        ShardedMap {
            shards: (0..n).map(|_| CountedMutex::new(std::collections::HashMap::new())).collect(),
            mask: (n - 1) as u64,
        }
    }

    #[inline]
    fn index(&self, k: u64) -> usize {
        (k & self.mask) as usize
    }

    /// The lock guarding a key that is not already a counter-derived integer.
    ///
    /// Hashes first, because the low bits of a `String` triple or a struct are
    /// not a round-robin the way an `InstanceId` is — `shard()`'s whole
    /// premise is that the caller's key spreads on its low bits, and reusing
    /// it for a hashed key would pile most of the map into a few shards.
    #[inline]
    pub fn shard_hashed<Q: std::hash::Hash + ?Sized>(
        &self,
        k: &Q,
    ) -> &CountedMutex<std::collections::HashMap<K, V>> {
        use std::hash::{BuildHasher, BuildHasherDefault};
        let h = BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default()
            .hash_one(k);
        // Fold the high bits down: `DefaultHasher`'s low bits are fine, but
        // mixing costs one xor and removes the question.
        &self.shards[self.index(h ^ (h >> 32))]
    }

    /// The lock guarding this key. The caller locks it and operates on the
    /// map inside, so a single-key operation touches exactly one shard.
    #[inline]
    pub fn shard(&self, k: u64) -> &CountedMutex<std::collections::HashMap<K, V>> {
        &self.shards[self.index(k)]
    }

    /// Every shard's contention, summed.
    ///
    /// One figure rather than 256, because the question this answers is "what
    /// did this map cost" and the answer has to stay comparable with the
    /// unsharded locks beside it in the same table.
    pub fn lock_stats(&self) -> LockSample {
        self.shards.iter().fold(LockSample::default(), |acc, s| {
            let x = s.stats().snapshot();
            LockSample {
                contended: acc.contended + x.contended,
                blocked_ns: acc.blocked_ns + x.blocked_ns,
            }
        })
    }

    /// Entries across all shards. Not on any dispatch path — it takes every
    /// lock in turn — and used only where the unsharded map's `len()` was.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
            m.shard(k).lock().insert(k, k as u32);
        }
        // Every shard got exactly one, so 64 threads on 64 sequential ids
        // never collide.
        assert_eq!(m.len(), 64);
        for s in m.shards.iter() {
            assert_eq!(s.lock().len(), 1, "a counter's low bits are a round-robin");
        }
        // And a key routes to the same shard every time, or `remove` would
        // miss what `insert` wrote.
        for k in 0..64u64 {
            assert_eq!(m.shard(k).lock().get(&k), Some(&(k as u32)));
            assert_eq!(m.shard(k).lock().remove(&k), Some(k as u32));
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
}
