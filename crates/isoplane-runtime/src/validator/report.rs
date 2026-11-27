//! Violation codes and what admission learned about an accepted module.

use isoplane_abi::imports;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationCode {
    /// The artifact belongs to a different local-debug execution mode.
    LocalDebugArtifact,
    /// **V1** — component types fail to match the declared world, or any
    /// import lies outside the world's allowlist.
    V1UndeclaredImport,
    /// **V1** — a required guest export is missing, or a mutable global is
    /// not exported, which leaves it outside what the host restores.
    V1MissingExport,
    /// **V1** — a declared import, or a required export, has the wrong
    /// signature. Name-only matching would be theatre: the world is a set of
    /// *types*, and half a type check is not a type check.
    V1SignatureMismatch,
    /// **V2** — the binary requires disabled engine features: threads, shared
    /// memory, atomics, relaxed SIMD, exceptions beyond the profile.
    /// `memory.atomic.wait*`/`notify` fail bytecode validation with the
    /// feature off, which is the point.
    V2ForbiddenFeature,
    /// **V3** — more than one linear memory, a shared memory, or a declared
    /// memory maximum exceeding the configured memory ceiling.
    V3MemoryShape,
    /// **V4** — structural resource-bomb heuristics: function count, local
    /// counts, table sizes, custom-section size beyond published limits.
    V4ResourceBomb,
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub code: ViolationCode,
    pub detail: String,
    /// The offending import or export, when the rule is about one.
    pub subject: Option<String>,
}

impl Violation {
    pub(super) fn new(code: ViolationCode, detail: impl Into<String>) -> Self {
        Violation {
            code,
            detail: detail.into(),
            subject: None,
        }
    }
    pub(super) fn about(
        code: ViolationCode,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Violation {
            code,
            detail: detail.into(),
            subject: Some(subject.into()),
        }
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.detail)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub optional_exports: Vec<&'static str>,
}

impl ValidationReport {
    pub fn has_initialize(&self) -> bool {
        self.optional_exports
            .contains(&imports::exports::INITIALIZE)
    }
}
