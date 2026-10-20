//! Pressure thresholds for handler reclamation.

pub use crate::hibernate_capture::{
    capture, delta_key, CaptureEnv, CaptureObserver, CaptureRefused, CaptureResult,
    GC_HINT_SNAPSHOT,
};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HibernateConfig {
    /// Begin reclaiming handlers below this percentage of available memory.
    pub reclaim_below_percent: u8,
    /// Stop reclaiming when available memory reaches this percentage.
    pub recover_above_percent: u8,
    /// Minimum returned-stack idle interval before handler capture.
    pub handler_after_ms: u64,
    /// Captured delta size above which the host emits an operator warning.
    pub delta_advisory_bytes: u64,
}

impl Default for HibernateConfig {
    fn default() -> Self {
        Self {
            reclaim_below_percent: 10,
            recover_above_percent: 15,
            handler_after_ms: 1_000,
            delta_advisory_bytes: 8 << 20,
        }
    }
}

impl HibernateConfig {
    /// Rejects invalid hysteresis thresholds before a cell begins serving.
    pub fn validate(&self) -> crate::Result<()> {
        anyhow::ensure!(
            self.reclaim_below_percent < self.recover_above_percent
                && self.recover_above_percent <= 100,
            "hibernation requires reclaim_below_percent < recover_above_percent <= 100"
        );
        Ok(())
    }

    /// Whether this sample calls for reclaiming, including recovery hysteresis.
    pub fn pressure_requested(
        &self,
        sample: crate::memory_pressure::MemorySample,
        reclaiming: bool,
    ) -> bool {
        let threshold = if reclaiming {
            self.recover_above_percent
        } else {
            self.reclaim_below_percent
        };
        sample.total.0 == 0
            || u128::from(sample.available.0) * 100
                < u128::from(sample.total.0) * u128::from(threshold)
    }
}
