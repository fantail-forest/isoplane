//! The hang breaker: repeated hard budget kills for one (module, route) fail
//! subsequent dispatches fast until the breaker cools or a new version
//! deploys. Isolation is already done by the time a kill happens; the breaker
//! exists to stop burning a whole CPU budget per dispatch to learn the same
//! thing again.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::ModuleId;

/// The key a breaker cools per. Version is part of `ModuleId`, so a deploy
/// produces a key that has never tripped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BreakerKey {
    pub module: ModuleId,
    pub route: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    /// Dispatches are refused without entering the guest.
    Open {
        until_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
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
        Self {
            kill_threshold: 3,
            window_ms: 10_000,
            cooldown_ms: 5_000,
        }
    }
}

#[derive(Debug, Default)]
struct Entry {
    kills: u32,
    first_kill_ms: u64,
    open_until_ms: u64,
    /// Set while the single probe dispatch after a cooldown is in flight, so a
    /// flood does not all get through at once.
    probing: bool,
}

pub struct HangBreaker {
    cfg: BreakerConfig,
    entries: Mutex<HashMap<BreakerKey, Entry>>,
}

impl HangBreaker {
    pub fn new(cfg: BreakerConfig) -> Self {
        Self {
            cfg,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// `Err(until_ms)` means fail fast: the dispatch never reaches an
    /// execution-pool thread.
    pub fn check(&self, key: &BreakerKey, now_ms: u64) -> Result<(), u64> {
        let mut g = self.entries.lock();
        let Some(e) = g.get_mut(key) else {
            return Ok(());
        };
        if e.open_until_ms == 0 {
            return Ok(());
        }
        if now_ms >= e.open_until_ms {
            // Cooled: exactly one probe goes through, and a kill of the probe
            // reopens the breaker on that kill alone.
            if e.probing {
                return Err(e.open_until_ms);
            }
            e.probing = true;
            return Ok(());
        }
        Err(e.open_until_ms)
    }

    /// A dispatch was hard-killed at its CPU budget.
    pub fn record_kill(&self, key: &BreakerKey, now_ms: u64) {
        let mut g = self.entries.lock();
        let e = g.entry(key.clone()).or_default();
        if e.probing {
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

    /// A dispatch completed normally, which closes the breaker.
    pub fn record_success(&self, key: &BreakerKey) {
        let mut g = self.entries.lock();
        if let Some(e) = g.get_mut(key) {
            e.kills = 0;
            e.open_until_ms = 0;
            e.probing = false;
        }
    }

    pub fn state(&self, key: &BreakerKey, now_ms: u64) -> BreakerState {
        let g = self.entries.lock();
        match g.get(key) {
            Some(e) if e.open_until_ms > now_ms => BreakerState::Open {
                until_ms: e.open_until_ms,
            },
            _ => BreakerState::Closed,
        }
    }

    /// An operator's manual override; a deploy clears the breaker on its own
    /// by producing a new key.
    pub fn clear_module(&self, module: &ModuleId) {
        self.entries.lock().retain(|k, _| &k.module != module);
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
        BreakerKey {
            module: ModuleId::new("app", "v1"),
            route: route.into(),
        }
    }

    #[test]
    fn trips_after_threshold_and_fails_fast() {
        let b = HangBreaker::new(BreakerConfig::default());
        let k = key("/spin");
        assert!(b.check(&k, 0).is_ok(), "a fresh key is closed");
        b.record_kill(&k, 0);
        b.record_kill(&k, 100);
        assert!(b.check(&k, 200).is_ok(), "two kills must not trip it");
        b.record_kill(&k, 200);
        assert_eq!(
            b.state(&k, 250),
            BreakerState::Open { until_ms: 5_200 },
            "the third kill opens it for the cooldown"
        );
        assert!(
            b.check(&k, 250).is_err(),
            "the third kill must fail subsequent dispatches fast"
        );
    }

    #[test]
    fn other_routes_are_unaffected() {
        let b = HangBreaker::default();
        let spin = key("/spin");
        for t in 0..3 {
            b.record_kill(&spin, t * 10);
        }
        assert!(b.check(&spin, 100).is_err(), "the offending route is open");
        assert!(
            b.check(&key("/healthz"), 100).is_ok(),
            "a hang on one route must not fail another"
        );
        assert!(
            b.check(
                &BreakerKey {
                    module: ModuleId::new("other", "v1"),
                    route: "/spin".into()
                },
                100
            )
            .is_ok(),
            "a hang in one module must not fail another"
        );
    }

    #[test]
    fn cools_and_lets_a_single_probe_through() {
        let b = HangBreaker::default();
        let k = key("/spin");
        for t in 0..3 {
            b.record_kill(&k, t);
        }
        assert!(b.check(&k, 1_000).is_err(), "open during the cooldown");
        assert!(b.check(&k, 6_000).is_ok(), "one probe after the cooldown");
        assert!(b.check(&k, 6_001).is_err(), "only one probe may pass");
        b.record_success(&k);
        assert_eq!(
            b.state(&k, 6_002),
            BreakerState::Closed,
            "a successful probe closes it"
        );
    }

    #[test]
    fn a_failed_probe_reopens_immediately() {
        let b = HangBreaker::default();
        let k = key("/spin");
        for t in 0..3 {
            b.record_kill(&k, t);
        }
        assert!(b.check(&k, 6_000).is_ok(), "the probe is admitted");
        b.record_kill(&k, 6_050);
        assert!(
            b.check(&k, 6_100).is_err(),
            "a killed probe reopens the breaker on that kill alone"
        );
    }

    #[test]
    fn a_new_version_starts_closed() {
        let b = HangBreaker::default();
        let v1 = key("/spin");
        for t in 0..3 {
            b.record_kill(&v1, t);
        }
        let v2 = BreakerKey {
            module: ModuleId::new("app", "v2"),
            route: "/spin".into(),
        };
        assert!(
            b.check(&v2, 100).is_ok(),
            "the version is in the key, so a deploy starts closed"
        );
    }
}
