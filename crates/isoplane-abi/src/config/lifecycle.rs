//! The lifecycle tiers an instance moves through.

use serde::{Deserialize, Serialize};

/// The lifecycle tiers an instance moves through. Defined here because the
/// cell owns the transitions while the node status API reports their counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleTier {
    /// Executing on a pool thread.
    Hot,
    /// Empty stack, futures pending, memory resident.
    Parked,
    /// Resumable sandbox state is retained at a host return boundary.
    Hibernated,
    /// No resumable sandbox state is retained.
    Discarded,
}

impl LifecycleTier {
    pub const ALL: &'static [LifecycleTier] = &[
        LifecycleTier::Hot,
        LifecycleTier::Parked,
        LifecycleTier::Hibernated,
        LifecycleTier::Discarded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleTier::Hot => "hot",
            LifecycleTier::Parked => "parked",
            LifecycleTier::Hibernated => "hibernated",
            LifecycleTier::Discarded => "discarded",
        }
    }

    /// Whether an instance in this tier occupies linear memory.
    pub fn is_resident(self) -> bool {
        matches!(self, LifecycleTier::Hot | LifecycleTier::Parked)
    }

    /// The legal transitions. `Hibernated -> Parked` is absent because the
    /// only exit from `Hibernated` is a wake, which makes the instance `Hot`;
    /// nothing leaves `Discarded` because a fresh activation is a new instance.
    pub fn may_transition_to(self, next: Self) -> bool {
        use LifecycleTier::*;
        matches!(
            (self, next),
            (Hot, Parked)
                | (Hot, Discarded)
                | (Parked, Hot)
                | (Parked, Hibernated)
                | (Parked, Discarded)
                | (Hibernated, Hot)
        )
    }
}

impl std::fmt::Display for LifecycleTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hot_and_parked_hold_memory() {
        assert!(
            LifecycleTier::Hot.is_resident(),
            "a hot instance is in memory"
        );
        assert!(
            LifecycleTier::Parked.is_resident(),
            "a parked instance is in memory"
        );
        assert!(
            !LifecycleTier::Hibernated.is_resident(),
            "a hibernated instance is bytes in storage"
        );
        assert!(
            !LifecycleTier::Discarded.is_resident(),
            "a discarded instance holds nothing"
        );
    }
}
