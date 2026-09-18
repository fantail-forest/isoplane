use isoplane_engine::Completion;
use serde::{Deserialize, Serialize};

/// The worker operation that refused preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildStage {
    /// Reading and validating immutable inputs.
    Input,
    /// Constructing the worker-compatible engine.
    Engine,
    /// Running and preserving initialization.
    Initialization,
    /// Encoding the complete prepared artifact.
    Artifact,
}

/// Structured child refusal, distinct from a launcher failure or process crash.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerFailure {
    /// Operation that failed.
    pub stage: BuildStage,
    /// Guest execution outcome, when initialization entered the guest.
    pub completion: Option<Completion>,
    /// Diagnostic context; callers classify using the typed fields.
    pub detail: String,
}

impl WorkerFailure {
    /// Classify an operation failure without parsing its diagnostic text.
    pub fn new(stage: BuildStage, error: anyhow::Error) -> Self {
        let completion = match error.downcast_ref::<crate::PrepareError>() {
            Some(
                crate::PrepareError::Initialization { completion, .. }
                | crate::PrepareError::Unattributable { completion, .. },
            ) => Some(*completion),
            // Other preparation failures have no guest completion outcome.
            _ => None,
        };
        Self {
            stage,
            completion,
            detail: error.to_string().chars().take(4096).collect(),
        }
    }
}
