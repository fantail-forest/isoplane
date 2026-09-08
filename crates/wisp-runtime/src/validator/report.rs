//! Violation codes and the tenant-facing capability manifest report.

use wisp_abi::imports::{self, ImportDef};
use wisp_abi::pal::PalDef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationCode {
    /// **V1** — component types fail to match the declared world, or any
    /// import lies outside the world's allowlist.
    V1UndeclaredImport,
    /// **V1** — a required guest export is missing.
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
    /// memory maximum exceeding the plan's ceiling.
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

impl Violation {
    pub fn parts(&self) -> (ViolationCode, Option<&str>, &str) {
        (self.code, self.subject.as_deref(), &self.detail)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    /// Every capability the module requested, in table order, including
    /// those reached through a delegated WASI import.
    pub capabilities: Vec<&'static str>,
    pub memory_pages_max: Option<u32>,
    pub function_count: u32,
    pub module_bytes: u64,
    /// Declared initial size of the linear memory, in 64 KiB pages.
    pub memory_pages_initial: Option<u32>,
    pub custom_section_bytes: u64,
    pub optional_exports: Vec<&'static str>,
    /// Every `wasi:*` import the PAL adapter composes for this module, in
    /// table order. Separate from `capabilities` so the door a capability
    /// arrived through stays visible.
    pub pal: Vec<&'static PalDef>,
}

impl ValidationReport {
    pub fn has_initialize(&self) -> bool {
        self.optional_exports
            .contains(&imports::exports::INITIALIZE)
    }

    /// The capability manifest as a tenant reads it: powers granted by
    /// group, what was not requested, and what arrived through the PAL.
    pub fn manifest(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "capability manifest ({} imports declared):",
            self.capabilities.len()
        ));
        let mut groups: Vec<(&'static str, Vec<&'static ImportDef>)> = Vec::new();
        for def in self.capabilities.iter().filter_map(|n| imports::lookup(n)) {
            let g = group_label(def);
            match groups.iter_mut().find(|(l, _)| *l == g) {
                Some((_, v)) => v.push(def),
                None => groups.push((g, vec![def])),
            }
        }
        for (label, defs) in &groups {
            let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
            lines.push(format!("  {label:<14} {}", names.join(", ")));
        }
        let denied: Vec<&str> = imports::IMPORTS
            .iter()
            .filter(|d| !self.capabilities.contains(&d.name))
            .map(|d| d.name)
            .collect();
        lines.push(format!(
            "  {:<14} {} imports NOT requested and therefore unreachable",
            "denied",
            denied.len()
        ));
        if !self.pal.is_empty() {
            let delegated: Vec<&&PalDef> = self
                .pal
                .iter()
                .filter(|p| p.disposition.is_delegated())
                .collect();
            let trapped = self.pal.len() - delegated.len();
            lines.push(format!(
                "  {:<14} {} wasi:* imports composed at deploy (PAL adapter):",
                "wasi-pal",
                self.pal.len()
            ));
            for p in &delegated {
                let to = match p.disposition.capability() {
                    Some(cap) => format!("delegates to `{cap}`"),
                    None => "answers empty; no such capability exists".to_string(),
                };
                lines.push(format!("  {:<14}   {} -> {to}", "", p.full_name()));
            }
            let mut ifaces: Vec<&str> = self
                .pal
                .iter()
                .filter(|p| !p.disposition.is_delegated())
                .map(|p| p.module)
                .collect();
            ifaces.sort_unstable();
            ifaces.dedup();
            lines.push(format!(
                "  {:<14}   {trapped} trap-on-call across {} interface(s): {}",
                "",
                ifaces.len(),
                ifaces.join(", ")
            ));
        }
        if let Some(max) = self.memory_pages_max {
            lines.push(format!(
                "  {:<14} 1 linear memory, {} .. {} pages ({} MiB max)",
                "memory",
                self.memory_pages_initial.unwrap_or(0),
                max,
                (max as u64 * 65_536) >> 20
            ));
        }
        lines.join("\n") + "\n"
    }
}

fn group_label(d: &ImportDef) -> &'static str {
    use wisp_abi::imports::Group::*;
    match d.group {
        Core => "core",
        Observe => "observability",
        Clock => "clock",
        Entropy => "entropy",
        Http => "egress:http",
        Response => "response",
        Socket => "sockets",
        Ctx => "ctx",
    }
}
