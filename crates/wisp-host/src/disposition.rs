//! What happens to an instance at the end of a request; [`Disposition`] has one producer, [`decide`].

use wisp_abi::config::HandlerLifecycle;
use wisp_engine::Completion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    /// Keep the instance with its residue. Only reachable under `reuse`.
    Warm,
    /// Back to template contents, then pooled; indistinguishable from a
    /// fresh clone, so a `fresh` acquisition may draw from this pool.
    Reset,
    Discard,
}

/// Constructible only by [`decide`], [`evict`] and [`policy_reset`], none of
/// which produce `Warm` for a `fresh` app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "an undisposed instance would leak residue, which `fresh` forbids"]
pub struct Disposition(DispositionKind);

impl Disposition {
    pub fn kind(self) -> DispositionKind {
        self.0
    }
    pub fn is_warm(self) -> bool {
        self.0 == DispositionKind::Warm
    }
    pub fn label(self) -> &'static str {
        match self.0 {
            DispositionKind::Warm => "warm",
            DispositionKind::Reset => "reset",
            DispositionKind::Discard => "discard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestEnd {
    /// The guest returned `pending`; a wake will resume the request.
    Parked,
    /// The response is delivered but `WaitUntil` work is outstanding; under
    /// `fresh` the reset is still owed.
    RetainedForWaitUntil,
    Disposed(Disposition),
}

/// The disposition table. A trapped or killed instance is discarded; `fresh`
/// never yields `Warm`; `reuse` yields `Warm`.
pub fn decide(
    lifecycle: HandlerLifecycle,
    completion: Completion,
    wait_until_outstanding: bool,
) -> RequestEnd {
    match completion {
        Completion::Parked => RequestEnd::Parked,
        Completion::Trapped | Completion::BudgetKill => {
            RequestEnd::Disposed(Disposition(DispositionKind::Discard))
        }
        // Cooperative termination resets dispatch state under either reuse policy.
        Completion::CleanKill => RequestEnd::Disposed(Disposition(DispositionKind::Reset)),
        Completion::Done => {
            if wait_until_outstanding {
                return RequestEnd::RetainedForWaitUntil;
            }
            match lifecycle {
                HandlerLifecycle::Fresh => {
                    RequestEnd::Disposed(Disposition(DispositionKind::Reset))
                }
                HandlerLifecycle::Reuse => RequestEnd::Disposed(Disposition(DispositionKind::Warm)),
            }
        }
    }
}

pub fn evict() -> Disposition {
    Disposition(DispositionKind::Discard)
}

/// A reset requested by policy rather than by the end of a request.
pub fn policy_reset() -> Disposition {
    Disposition(DispositionKind::Reset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_never_decides_warm() {
        use Completion::*;
        for c in [Done, Parked, Trapped, BudgetKill] {
            for wait in [false, true] {
                let end = decide(HandlerLifecycle::Fresh, c, wait);
                if let RequestEnd::Disposed(d) = end {
                    assert!(!d.is_warm(), "fresh produced Warm for {c:?} wait={wait}");
                }
            }
        }
        assert_eq!(
            decide(HandlerLifecycle::Fresh, Done, false),
            RequestEnd::Disposed(Disposition(DispositionKind::Reset)),
            "a completed fresh dispatch resets"
        );
        assert_eq!(
            decide(HandlerLifecycle::Fresh, Done, true),
            RequestEnd::RetainedForWaitUntil,
            "the reset is owed only after WaitUntil work completes"
        );
        assert_eq!(
            decide(HandlerLifecycle::Reuse, Done, false),
            RequestEnd::Disposed(Disposition(DispositionKind::Warm)),
            "a completed reuse dispatch stays warm"
        );
        for lc in [HandlerLifecycle::Fresh, HandlerLifecycle::Reuse] {
            assert_eq!(
                decide(lc, BudgetKill, false),
                RequestEnd::Disposed(Disposition(DispositionKind::Discard)),
                "a killed instance is never reused"
            );
            assert_eq!(
                decide(lc, Trapped, false),
                RequestEnd::Disposed(Disposition(DispositionKind::Discard)),
                "a trapped instance is never reused"
            );
            assert_eq!(
                decide(lc, Parked, false),
                RequestEnd::Parked,
                "a parked dispatch is still in flight"
            );
        }
    }
}
