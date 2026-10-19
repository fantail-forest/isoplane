//! What happens to an instance at the end of a dispatch; [`Disposition`] has one producer, [`decide`].

use isoplane_engine::Completion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    /// Keep the instance with its residue.
    Warm,
    Discard,
}

/// Constructible only by [`decide`], [`evict`] and [`warm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "an undisposed instance would never return to its pool"]
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
            DispositionKind::Discard => "discard",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchEnd {
    /// The guest returned `pending`; a wake will resume the dispatch.
    Parked,
    /// The streaming response is unfinished; the dispatch ends when it finishes.
    RetainedForStreaming,
    Disposed(Disposition),
}

/// The disposition table. A trapped or killed instance is discarded; one whose
/// dispatch completed or exited after cancellation stays warm.
pub fn decide(completion: Completion, streaming_open: bool) -> DispatchEnd {
    match completion {
        Completion::Parked => DispatchEnd::Parked,
        Completion::Trapped | Completion::BudgetKill => {
            DispatchEnd::Disposed(Disposition(DispositionKind::Discard))
        }
        Completion::Done if streaming_open => DispatchEnd::RetainedForStreaming,
        Completion::Done | Completion::BudgetExit => {
            DispatchEnd::Disposed(Disposition(DispositionKind::Warm))
        }
    }
}

pub fn evict() -> Disposition {
    Disposition(DispositionKind::Discard)
}

/// An idle instance returned unchanged, such as one taken for a dispatch that
/// never ran.
pub fn warm() -> Disposition {
    Disposition(DispositionKind::Warm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_completion_has_its_disposition() {
        use Completion::*;
        assert_eq!(
            decide(Done, false),
            DispatchEnd::Disposed(Disposition(DispositionKind::Warm)),
            "a completed dispatch stays warm"
        );
        assert_eq!(
            decide(Done, true),
            DispatchEnd::RetainedForStreaming,
            "an unfinished streaming response retains its dispatch"
        );
        assert_eq!(
            decide(BudgetExit, false),
            DispatchEnd::Disposed(Disposition(DispositionKind::Warm)),
            "an instance whose dispatch exited after cancellation stays warm"
        );
        assert_eq!(
            decide(BudgetKill, false),
            DispatchEnd::Disposed(Disposition(DispositionKind::Discard)),
            "a killed instance is never reused"
        );
        assert_eq!(
            decide(Trapped, false),
            DispatchEnd::Disposed(Disposition(DispositionKind::Discard)),
            "a trapped instance is never reused"
        );
        assert_eq!(
            decide(Parked, false),
            DispatchEnd::Parked,
            "a parked dispatch is still in flight"
        );
    }
}
