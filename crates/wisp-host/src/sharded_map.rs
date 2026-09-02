//! The map behind `Cell::active`: one mutex per shard, because every use is a
//! single-key operation and nothing holds an invariant across two keys.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;

/// `InstanceId`s come from a counter, so their low bits already spread
/// evenly; a key type whose low bits do not folds its own hash in here.
pub(crate) trait ShardKey {
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

#[repr(align(128))]
struct Shard<K, V> {
    map: Mutex<HashMap<K, V>>,
}

pub(crate) struct ShardedMap<K, V> {
    shards: Box<[Shard<K, V>]>,
    mask: u64,
}

impl<K: ShardKey + Hash + Eq, V> ShardedMap<K, V> {
    /// `shards` is rounded up to a power of two.
    pub(crate) fn new(shards: usize) -> ShardedMap<K, V> {
        let n = shards.max(1).next_power_of_two();
        ShardedMap {
            shards: (0..n)
                .map(|_| Shard {
                    map: Mutex::new(HashMap::new()),
                })
                .collect(),
            mask: (n - 1) as u64,
        }
    }

    #[inline]
    fn shard(&self, k: &K) -> &Mutex<HashMap<K, V>> {
        &self.shards[(k.shard_key() & self.mask) as usize].map
    }

    pub(crate) fn contains_key(&self, k: &K) -> bool {
        self.shard(k).lock().contains_key(k)
    }

    pub(crate) fn insert(&self, k: K, v: V) -> Option<V> {
        self.shard(&k).lock().insert(k, v)
    }

    pub(crate) fn remove(&self, k: &K) -> Option<V> {
        self.shard(k).lock().remove(k)
    }

    /// `f` runs with the shard locked, so a reference never leaves the lock.
    pub(crate) fn with<R>(&self, k: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
        self.shard(k).lock().get(k).map(f)
    }

    pub(crate) fn with_mut<R>(&self, k: &K, f: impl FnOnce(&mut V) -> R) -> Option<R> {
        self.shard(k).lock().get_mut(k).map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_keys_spread_across_the_shards_and_route_back_to_their_own() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(64);
        for k in 0..64u64 {
            assert_eq!(
                m.insert(k, k as u32),
                None,
                "a fresh key must not displace another"
            );
        }
        for s in m.shards.iter() {
            assert_eq!(
                s.map.lock().len(),
                1,
                "a counter's low bits are a round-robin"
            );
        }
        for k in 0..64u64 {
            assert_eq!(
                m.with(&k, |v| *v),
                Some(k as u32),
                "a key must read from its own shard"
            );
            assert_eq!(
                m.with_mut(&k, |v| {
                    *v += 1;
                    *v
                }),
                Some(k as u32 + 1),
                "a key must update in its own shard"
            );
            assert_eq!(
                m.remove(&k),
                Some(k as u32 + 1),
                "a key must remove from its own shard"
            );
            assert!(!m.contains_key(&k), "a removed key must be gone");
        }
    }

    #[test]
    fn a_shard_count_rounds_up_to_a_power_of_two() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(100);
        assert_eq!(
            m.shards.len(),
            128,
            "the mask only works over a power of two"
        );
        assert_eq!(m.mask, 127, "the mask covers every shard");
        let m: ShardedMap<u64, u32> = ShardedMap::new(0);
        assert_eq!(
            m.shards.len(),
            1,
            "zero shards is one shard, not a divide by zero"
        );
    }

    #[test]
    fn no_two_shards_share_a_cacheline() {
        let m: ShardedMap<u64, u32> = ShardedMap::new(8);
        let addr = |i: usize| std::ptr::addr_of!(m.shards[i]) as usize;
        assert_eq!(
            addr(0) % 128,
            0,
            "the shard array does not start on a line boundary"
        );
        for i in 1..8 {
            assert!(
                addr(i) - addr(i - 1) >= 128,
                "shards {} and {} are {} bytes apart, so two threads on two different shards \
                 write one line",
                i - 1,
                i,
                addr(i) - addr(i - 1)
            );
        }
    }
}
