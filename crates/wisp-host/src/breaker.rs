//! The hang breaker (spec.md §6.4, claim C7).
//!
//! > An instance killed at budget is discarded; repeated budget kills for the
//! > same (module, route) trip a breaker that fails subsequent dispatches fast
//! > until the breaker cools or a new version deploys — the same posture as
//! > the Workers runtime's hung-worker detection.
//!
//! The breaker is *not* an isolation mechanism. §8.3 is explicit that the
//! isolation is already done by the time we get here: a spinner occupies only
//! its own execution-pool thread and the kernel preempts it, so the blast
//! radius is the offender's own requests. The breaker exists to stop paying
//! 50 ms of CPU per request to learn the same thing repeatedly.

use std::collections::HashMap;


use crate::ModuleId;

/// The key a breaker cools per. Version is part of `ModuleId`, so "a new
/// version deploys" resets the breaker for free — a deploy simply produces a
/// key that has never tripped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BreakerKey {
    pub module: ModuleId,
    pub route: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    /// Failing fast. Subsequent dispatches are refused without entering the
    /// guest at all, which is the point: no CPU is spent to relearn.
    Open { until_ms: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct BreakerConfig {
    /// Consecutive budget kills before the breaker opens.
    pub kill_threshold: u32,
    /// Window over which kills count as consecutive.
    pub window_ms: u64,
    /// How long the breaker stays open before a probe is allowed through.
    pub cooldown_ms: u64,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self { kill_threshold: 3, window_ms: 10_000, cooldown_ms: 5_000 }
    }
}

#[derive(Debug, Default)]
struct Entry {
    kills: u32,
    first_kill_ms: u64,
    open_until_ms: u64,
    /// Set while a single probe dispatch is in flight after cooldown, so a
    /// flood does not all get through at once.
    probing: bool,
}

pub struct HangBreaker {
    cfg: BreakerConfig,
    entries: crate::lockstats::CountedMutex<HashMap<BreakerKey, Entry>>,
}

impl HangBreaker {
    /// What waiting for the breaker table cost. See `lockstats`.
    pub fn lock_stats(&self) -> crate::lockstats::LockSample {
        self.entries.stats().snapshot()
    }

    pub fn new(cfg: BreakerConfig) -> Self {
        Self { cfg, entries: crate::lockstats::CountedMutex::new(HashMap::new()) }
    }

    /// Admission check. `Err(until_ms)` means fail fast — the dispatch never
    /// reaches an execution-pool thread.
    pub fn check(&self, key: &BreakerKey, now_ms: u64) -> Result<(), u64> {
        self.check_marked(key, now_ms, &mut None)
    }

    /// [`HangBreaker::check`], marking the wait for `entries` separately from
    /// the work done under it.
    ///
    /// **A triple.** The hold is ~34 ns of hashing three strings against a
    /// mark that costs 20 ns on this machine, so the wait and the hold are
    /// separable. It was briefly classified as a pair on the strength of a
    /// calibration that reported the mark cost as 40 ns; that calibration was
    /// measuring a core fresh out of a sleep and is fixed.
    pub fn check_marked(
        &self,
        key: &BreakerKey,
        now_ms: u64,
        marks: &mut Option<crate::marks::RequestMarks>,
    ) -> Result<(), u64> {
        let mut g = self.entries.lock();
        crate::marks::mark(marks, crate::marks::Mark::BreakerLockAcquired);
        let Some(e) = g.get_mut(key) else { return Ok(()) };
        if e.open_until_ms == 0 {
            return Ok(());
        }
        if now_ms >= e.open_until_ms {
            // Cooled. Let exactly one probe through; if it is killed too, the
            // breaker re-opens on the first kill rather than after three.
            if e.probing {
                return Err(e.open_until_ms);
            }
            e.probing = true;
            return Ok(());
        }
        Err(e.open_until_ms)
    }

    /// A dispatch was killed at its CPU budget (§2.5.8: abrupt, no unwinding).
    pub fn record_kill(&self, key: &BreakerKey, now_ms: u64) {
        let mut g = self.entries.lock();
        let e = g.entry(key.clone()).or_default();
        if e.probing {
            // The probe failed. Straight back open.
            e.probing = false;
            e.open_until_ms = now_ms + self.cfg.cooldown_ms;
            return;
        }
        if e.kills == 0 || now_ms.saturating_sub(e.first_kill_ms) > self.cfg.window_ms {
            e.kills = 1;
            e.first_kill_ms = now_ms;
        } else {
            e.kills += 1;
        }
        if e.kills >= self.cfg.kill_threshold {
            e.open_until_ms = now_ms + self.cfg.cooldown_ms;
        }
    }

    /// A dispatch completed normally. Success closes the breaker: the tenant
    /// pushed a fix, or the hang was input-dependent.
    pub fn record_success(&self, key: &BreakerKey) {
        self.record_success_marked(key, &mut None)
    }

    /// [`HangBreaker::record_success`], marking the wait for `entries`
    /// separately. A **triple**, for the same reason as `check_marked`.
    pub fn record_success_marked(
        &self,
        key: &BreakerKey,
        marks: &mut Option<crate::marks::RequestMarks>,
    ) {
        let mut g = self.entries.lock();
        crate::marks::mark(marks, crate::marks::Mark::BreakerSuccessAcquired);
        if let Some(e) = g.get_mut(key) {
            e.kills = 0;
            e.open_until_ms = 0;
            e.probing = false;
        }
    }

    pub fn state(&self, key: &BreakerKey, now_ms: u64) -> BreakerState {
        let g = self.entries.lock();
        match g.get(key) {
            Some(e) if e.open_until_ms > now_ms => BreakerState::Open { until_ms: e.open_until_ms },
            _ => BreakerState::Closed,
        }
    }

    /// §6.4: "...until the breaker cools **or a new version deploys**". A new
    /// version has a different `ModuleId`, so this only exists for the case
    /// where an operator wants to clear a key by hand.
    pub fn clear_module(&self, module: &ModuleId) {
        self.entries.lock().retain(|k, _| &k.module != module);
    }

    pub fn open_count(&self, now_ms: u64) -> usize {
        self.entries.lock().values().filter(|e| e.open_until_ms > now_ms).count()
    }
}

impl Default for HangBreaker {
    fn default() -> Self {
        Self::new(BreakerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(route: &str) -> BreakerKey {
        BreakerKey { module: ModuleId::new("app", "v1"), route: route.into() }
    }

    #[test]
    fn trips_after_threshold_and_fails_fast() {
        let b = HangBreaker::new(BreakerConfig::default());
        let k = key("/spin");
        assert!(b.check(&k, 0).is_ok());
        b.record_kill(&k, 0);
        b.record_kill(&k, 100);
        assert!(b.check(&k, 200).is_ok(), "two kills must not trip it");
        b.record_kill(&k, 200);
        assert_eq!(b.state(&k, 250), BreakerState::Open { until_ms: 5_200 });
        assert!(b.check(&k, 250).is_err(), "third kill must fail subsequent dispatches fast");
    }

    /// The breaker is per (module, route): a hang on `/spin` must not fail
    /// `/healthz`. §8.3's blast radius is "the offender's own requests", and
    /// the breaker must not widen it.
    #[test]
    fn other_routes_are_unaffected() {
        let b = HangBreaker::default();
        let spin = key("/spin");
        for t in 0..3 {
            b.record_kill(&spin, t * 10);
        }
        assert!(b.check(&spin, 100).is_err());
        assert!(b.check(&key("/healthz"), 100).is_ok());
        assert!(b.check(&BreakerKey { module: ModuleId::new("other", "v1"), route: "/spin".into() }, 100).is_ok());
    }

    #[test]
    fn cools_and_lets_a_single_probe_through() {
        let b = HangBreaker::default();
        let k = key("/spin");
        for t in 0..3 {
            b.record_kill(&k, t);
        }
        assert!(b.check(&k, 1_000).is_err());
        // After the cooldown one probe is admitted, and only one.
        assert!(b.check(&k, 6_000).is_ok());
        assert!(b.check(&k, 6_001).is_err(), "only one probe may pass");
        b.record_success(&k);
        assert_eq!(b.state(&k, 6_002), BreakerState::Closed);
    }

    #[test]
    fn a_failed_probe_reopens_immediately() {
        let b = HangBreaker::default();
        let k = key("/spin");
        for t in 0..3 {
            b.record_kill(&k, t);
        }
        assert!(b.check(&k, 6_000).is_ok());
        b.record_kill(&k, 6_050);
        assert!(b.check(&k, 6_100).is_err());
    }

    /// §9.4/§6.4: a new version deploys and the breaker is gone, because the
    /// key contains the version.
    #[test]
    fn a_new_version_starts_closed() {
        let b = HangBreaker::default();
        let v1 = key("/spin");
        for t in 0..3 {
            b.record_kill(&v1, t);
        }
        let v2 = BreakerKey { module: ModuleId::new("app", "v2"), route: "/spin".into() };
        assert!(b.check(&v2, 100).is_ok());
    }
}
