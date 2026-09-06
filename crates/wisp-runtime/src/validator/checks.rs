//! The V1-V4 checks: imports, exports, memory shape, and structural resource-bomb limits.

use std::collections::BTreeSet;

use wasmparser::{ExternalKind, TypeRef};
use wisp_abi::config::Limits;
use wisp_abi::imports;
use wisp_abi::pal::{self, Disposition, PalDef};

use super::report::{ValidationReport, Violation, ViolationCode};
use super::scan::{
    func_type_index, kind_label, kind_matches, render_sig, render_want, sig_matches, Scan,
};

pub(super) fn check_imports(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    if s.nested {
        v.push(Violation::new(
            ViolationCode::V1UndeclaredImport,
            "module contains a nested module/component section; the artifact is a single \
             core module and nested instantiation is a linking escape hatch (V1)",
        ));
    }
    if s.imports.len() as u32 > limits::MAX_IMPORTS {
        v.push(Violation::new(
            ViolationCode::V4ResourceBomb,
            format!(
                "{} imports exceeds MAX_IMPORTS={}",
                s.imports.len(),
                limits::MAX_IMPORTS
            ),
        ));
    }

    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for (module, field, ty) in &s.imports {
        let Some(def) = (if module == imports::HOST_MODULE {
            imports::lookup(field)
        } else {
            None
        }) else {
            match pal::lookup(module, field) {
                Some(p) => {
                    check_pal_import(p, ty, s, &mut seen, report, v);
                    continue;
                }
                None => {
                    v.push(Violation::about(
                        ViolationCode::V1UndeclaredImport,
                        format!("{module}.{field}"),
                        if pal::is_wasi(module) {
                            format!(
                                "import `{module}`.`{field}` is a WASI import outside the \
                                 enumerated PAL whitelist — the adapter composes only \
                                 the {} `wasi:*` functions it can satisfy without adding a \
                                 capability or an information channel, and this is not one of \
                                 them (V1)",
                                pal::PAL_IMPORTS.len()
                            )
                        } else {
                            format!(
                                "import `{module}`.`{field}` is not in the wisp:platform@1 \
                                 capability table — an undeclared import is an attempted \
                                 capability (V1)"
                            )
                        },
                    ));
                    continue;
                }
            }
        };

        match func_type_index(ty) {
            Some(idx) => match s.func_types.get(idx as usize) {
                Some(Some((params, results))) => {
                    if sig_matches(&def.sig, params, results) {
                        seen.insert(def.name);
                    } else {
                        v.push(Violation::about(
                            ViolationCode::V1SignatureMismatch,
                            def.name,
                            format!(
                                "import `wisp`.`{}` has signature {}, world declares {}",
                                def.name,
                                render_sig(params, results),
                                render_want(&def.sig)
                            ),
                        ));
                    }
                }
                _ => v.push(Violation::new(
                    ViolationCode::V1SignatureMismatch,
                    format!(
                        "import `wisp`.`{}` references non-function type {idx}",
                        def.name
                    ),
                )),
            },
            None => v.push(Violation::new(
                ViolationCode::V1SignatureMismatch,
                format!(
                    "import `wisp`.`{}` is declared as {}; every entry in the world is a \
                     function, and a host memory, table or global inside the sandbox would be \
                     a host pointer in guest memory",
                    def.name,
                    kind_label(ty)
                ),
            )),
        }
    }

    // Table order, not module order: the manifest is the platform's document.
    report.capabilities = imports::IMPORTS
        .iter()
        .map(|d| d.name)
        .filter(|n| seen.contains(n))
        .collect();
    report.pal.sort_by_key(|p| {
        pal::PAL_IMPORTS
            .iter()
            .position(|q| q.module == p.module && q.func == p.func)
            .unwrap_or(usize::MAX)
    });
}

/// A delegated entry runs a body against its declared type, so its
/// signature is checked; a trap-on-call stub is generated from the module's
/// own type and traps before it reads an argument.
fn check_pal_import(
    p: &'static PalDef,
    ty: &TypeRef,
    s: &Scan,
    seen: &mut BTreeSet<&'static str>,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
) {
    let Some(idx) = func_type_index(ty) else {
        v.push(Violation::about(
            ViolationCode::V1SignatureMismatch,
            p.full_name(),
            format!(
                "`{}` is imported as a memory/table/global; every entry the PAL adapter \
                 composes is a function: no host objects inside the sandbox",
                p.full_name()
            ),
        ));
        return;
    };

    if let (Disposition::Delegate(_), Some(want)) = (p.disposition, p.sig) {
        match s.func_types.get(idx as usize) {
            Some(Some((params, results))) if sig_matches(&want, params, results) => {}
            Some(Some((params, results))) => {
                v.push(Violation::about(
                    ViolationCode::V1SignatureMismatch,
                    p.full_name(),
                    format!(
                        "`{}` has signature {}, but the PAL adapter delegates it with {} — a \
                         delegated import runs a real body against its declared type, so half \
                         a type check is not a type check",
                        p.full_name(),
                        render_sig(params, results),
                        render_want(&want)
                    ),
                ));
                return;
            }
            _ => {
                v.push(Violation::new(
                    ViolationCode::V1SignatureMismatch,
                    format!("`{}` references non-function type {idx}", p.full_name()),
                ));
                return;
            }
        }
    }

    if let Some(def) = p.disposition.capability().and_then(imports::lookup) {
        seen.insert(def.name);
    }
    report.pal.push(p);
}

pub(super) fn check_exports(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    for required in imports::exports::REQUIRED {
        let want_kind = if *required == imports::exports::MEMORY {
            ExternalKind::Memory
        } else {
            ExternalKind::Func
        };
        match s.exports.iter().find(|(n, _, _)| n == required) {
            Some((_, k, idx)) if kind_matches(*k, want_kind) => {
                if want_kind == ExternalKind::Func {
                    if let Some(want) = imports::exports::signature(required) {
                        match s
                            .func_type_of
                            .get(*idx as usize)
                            .and_then(|t| s.func_types.get(*t as usize))
                        {
                            Some(Some((params, results)))
                                if sig_matches(&want, params, results) => {}
                            Some(Some((params, results))) => v.push(Violation::about(
                                ViolationCode::V1SignatureMismatch,
                                *required,
                                format!(
                                    "export `{required}` has signature {}, world declares {}",
                                    render_sig(params, results),
                                    render_want(&want)
                                ),
                            )),
                            _ => v.push(Violation::new(
                                ViolationCode::V1SignatureMismatch,
                                format!("export `{required}` does not resolve to a function type"),
                            )),
                        }
                    }
                }
            }
            Some((_, k, _)) => v.push(Violation::new(
                ViolationCode::V1MissingExport,
                format!("export `{required}` is a {k:?}, expected {want_kind:?}"),
            )),
            None => v.push(Violation::about(
                ViolationCode::V1MissingExport,
                *required,
                format!(
                    "required guest export `{required}` is missing — the host has no way to \
                     enter this module"
                ),
            )),
        }
    }
    report.optional_exports = imports::exports::OPTIONAL
        .iter()
        .copied()
        .filter(|o| {
            s.exports
                .iter()
                .any(|(n, k, _)| n == o && kind_matches(*k, ExternalKind::Func))
        })
        .collect();

    if s.exports.len() as u32 > limits::MAX_EXPORTS {
        v.push(Violation::new(
            ViolationCode::V4ResourceBomb,
            format!(
                "{} exports exceeds MAX_EXPORTS={}",
                s.exports.len(),
                limits::MAX_EXPORTS
            ),
        ));
    }
}

pub(super) fn check_memory(
    s: &Scan,
    budgets: Limits,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
) {
    let ceiling_bytes = budgets.instance_memory_max;
    let ceiling_pages = ceiling_bytes / wisp_abi::WASM_PAGE_SIZE as u64;

    if s.memories.len() != 1 {
        v.push(Violation::new(
            ViolationCode::V3MemoryShape,
            format!(
                "module declares {} linear memories; exactly one is permitted. \
                 One memory is what makes the template image, the CoW clone and the \
                 hibernation page-diff a single object rather than a set",
                s.memories.len()
            ),
        ));
    }

    for m in &s.memories {
        if m.shared {
            v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                "linear memory is declared `shared` — threads and shared memory are off \
                 (V2/V3: no shared memory means no counting thread)",
            ));
        }
        if m.memory64 {
            v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                "64-bit linear memory is outside the engine profile",
            ));
        }
        match m.maximum {
            Some(max) if max > ceiling_pages => v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                format!(
                    "declared memory maximum {} pages ({} MiB) exceeds the application memory \
                     ceiling of {} pages ({} MiB)",
                    max,
                    (max * 65_536) >> 20,
                    ceiling_pages,
                    ceiling_bytes >> 20
                ),
            )),
            _ => {}
        }
        if m.initial > ceiling_pages {
            v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                format!(
                    "declared memory minimum {} pages exceeds the application memory ceiling of {} \
                     pages — the module could never be instantiated",
                    m.initial, ceiling_pages
                ),
            ));
        }
    }

    if let Some(m) = s.memories.first() {
        report.memory_pages_initial = Some(m.initial.min(u32::MAX as u64) as u32);
        report.memory_pages_max = m.maximum.map(|x| x.min(u32::MAX as u64) as u32);
    }
}

pub(super) fn check_bombs(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    report.function_count = s.imported_funcs.saturating_add(s.defined_funcs);
    report.custom_section_bytes = s.custom_section_bytes;

    let mut bomb = |cond: bool, detail: String| {
        if cond {
            v.push(Violation::new(ViolationCode::V4ResourceBomb, detail));
        }
    };
    bomb(
        report.function_count > limits::MAX_FUNCTIONS,
        format!(
            "{} functions exceeds MAX_FUNCTIONS={} — AOT compile time is superlinear in \
             function count and compile bombs are a real attack",
            report.function_count,
            limits::MAX_FUNCTIONS
        ),
    );
    bomb(
        s.max_locals > limits::MAX_LOCALS_PER_FUNCTION,
        format!(
            "function #{} declares {} locals, exceeding MAX_LOCALS_PER_FUNCTION={}",
            s.max_locals_fn,
            s.max_locals,
            limits::MAX_LOCALS_PER_FUNCTION
        ),
    );
    bomb(
        s.table_elements > limits::MAX_TABLE_ELEMENTS as u64,
        format!(
            "table space of {} elements exceeds MAX_TABLE_ELEMENTS={}",
            s.table_elements,
            limits::MAX_TABLE_ELEMENTS
        ),
    );
    bomb(
        s.custom_section_bytes > limits::MAX_CUSTOM_SECTION_BYTES,
        format!(
            "{} bytes of custom sections exceeds MAX_CUSTOM_SECTION_BYTES={} (largest: `{}`, \
             {} bytes)",
            s.custom_section_bytes,
            limits::MAX_CUSTOM_SECTION_BYTES,
            s.largest_custom.0,
            s.largest_custom.1
        ),
    );
    bomb(
        s.type_count > limits::MAX_TYPES,
        format!(
            "{} types exceeds MAX_TYPES={}",
            s.type_count,
            limits::MAX_TYPES
        ),
    );
    bomb(
        s.element_segments > limits::MAX_ELEMENT_SEGMENTS,
        format!(
            "{} element segments exceeds MAX_ELEMENT_SEGMENTS={}",
            s.element_segments,
            limits::MAX_ELEMENT_SEGMENTS
        ),
    );
    bomb(
        s.data_segments > limits::MAX_DATA_SEGMENTS,
        format!(
            "{} data segments exceeds MAX_DATA_SEGMENTS={}",
            s.data_segments,
            limits::MAX_DATA_SEGMENTS
        ),
    );
}

/// The V4 limits, published so tenants can design against them.
pub mod limits {
    pub const MAX_FUNCTIONS: u32 = 200_000;
    pub const MAX_LOCALS_PER_FUNCTION: u32 = 50_000;
    pub const MAX_TABLE_ELEMENTS: u32 = 1_000_000;
    pub const MAX_CUSTOM_SECTION_BYTES: u64 = 16 << 20;
    pub const MAX_IMPORTS: u32 = 512;
    pub const MAX_EXPORTS: u32 = 512;
    pub const MAX_TYPES: u32 = 100_000;
    pub const MAX_ELEMENT_SEGMENTS: u32 = 100_000;
    pub const MAX_DATA_SEGMENTS: u32 = 100_000;
}
