//! The V1-V4 checks: imports, exports, global visibility, memory shape, and structural
//! resource-bomb limits.

use std::collections::BTreeSet;

use isoplane_abi::pal::{self, Disposition, PalDef};
use isoplane_abi::{config::ValidationLimits, imports};
use wasmparser::{ExternalKind, TypeRef};

use super::report::{ValidationReport, Violation, ViolationCode};
use super::scan::{
    func_type_index, kind_label, kind_matches, render_sig, render_want, sig_matches, Scan,
};

pub(super) fn check_imports(
    s: &Scan,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
    policy: &ValidationLimits,
) {
    if s.nested {
        v.push(Violation::new(
            ViolationCode::V1UndeclaredImport,
            "module contains a nested module/component section; the artifact is a single \
             core module and nested instantiation is a linking escape hatch (V1)",
        ));
    }
    if s.imports.len() as u32 > policy.max_imports {
        v.push(Violation::new(
            ViolationCode::V4ResourceBomb,
            format!(
                "{} imports exceeds MAX_IMPORTS={}",
                s.imports.len(),
                policy.max_imports
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
                                "import `{module}`.`{field}` is not in the isoplane:platform@1 \
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
                                "import `isoplane`.`{}` has signature {}, world declares {}",
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
                        "import `isoplane`.`{}` references non-function type {idx}",
                        def.name
                    ),
                )),
            },
            None => v.push(Violation::new(
                ViolationCode::V1SignatureMismatch,
                format!(
                    "import `isoplane`.`{}` is declared as {}; every entry in the world is a \
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

pub(super) fn check_exports(
    s: &Scan,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
    policy: &ValidationLimits,
) {
    for name in imports::exports::REQUIRED
        .iter()
        .chain(imports::exports::OPTIONAL)
    {
        let want_kind = if *name == imports::exports::MEMORY {
            ExternalKind::Memory
        } else {
            ExternalKind::Func
        };
        match s.exports.iter().find(|(n, _, _)| n == name) {
            Some((_, k, idx)) if kind_matches(*k, want_kind) => {
                if want_kind == ExternalKind::Func {
                    if let Some(want) = imports::exports::signature(name) {
                        match s
                            .func_type_of
                            .get(*idx as usize)
                            .and_then(|t| s.func_types.get(*t as usize))
                        {
                            Some(Some((params, results)))
                                if sig_matches(&want, params, results) => {}
                            Some(Some((params, results))) => v.push(Violation::about(
                                ViolationCode::V1SignatureMismatch,
                                *name,
                                format!(
                                    "export `{name}` has signature {}, world declares {}",
                                    render_sig(params, results),
                                    render_want(&want)
                                ),
                            )),
                            _ => v.push(Violation::new(
                                ViolationCode::V1SignatureMismatch,
                                format!("export `{name}` does not resolve to a function type"),
                            )),
                        }
                    }
                }
            }
            Some((_, k, _)) => v.push(Violation::new(
                ViolationCode::V1MissingExport,
                format!("export `{name}` is a {k:?}, expected {want_kind:?}"),
            )),
            None if imports::exports::OPTIONAL.contains(name) => {}
            None => v.push(Violation::about(
                ViolationCode::V1MissingExport,
                *name,
                format!(
                    "required guest export `{name}` is missing — the host has no way to \
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

    if s.exports.len() as u32 > policy.max_exports {
        v.push(Violation::new(
            ViolationCode::V4ResourceBomb,
            format!(
                "{} exports exceeds MAX_EXPORTS={}",
                s.exports.len(),
                policy.max_exports
            ),
        ));
    }
}

/// The host reads and restores globals only through exports, so an unexported
/// mutable global would be state that neither a reset to the template nor a
/// hibernation capture can see.
pub(super) fn check_globals(s: &Scan, v: &mut Vec<Violation>) {
    let exported: BTreeSet<u32> = s
        .exports
        .iter()
        .filter(|(_, kind, _)| *kind == ExternalKind::Global)
        .map(|(_, _, index)| *index)
        .collect();
    for (index, _) in (0u32..)
        .zip(&s.global_mutable)
        .filter(|(index, mutable)| **mutable && !exported.contains(index))
    {
        let global = match s.global_names.get(&index) {
            Some(name) => format!("global {index} (`{name}`)"),
            None => format!("global {index}"),
        };
        v.push(Violation::about(
            ViolationCode::V1MissingExport,
            global.clone(),
            format!(
                "mutable {global} is not exported; the host restores a reset or hibernated \
                 instance's globals only through their exports, so a reset would not clear \
                 this global and hibernation would not capture it"
            ),
        ));
    }
}

pub(super) fn check_memory(
    s: &Scan,
    ceiling_bytes: u64,
    require_maximum: bool,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
) {
    let ceiling_pages = ceiling_bytes / isoplane_abi::WASM_PAGE_SIZE as u64;

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
            None if require_maximum => v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                "an explicit guest memory budget requires a declared WebAssembly memory maximum",
            )),
            Some(max) if max > ceiling_pages => v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                format!(
                    "declared memory maximum {} pages ({} MiB) exceeds the configured \
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
                    "declared memory minimum {} pages exceeds the configured ceiling of {} \
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

pub(super) fn check_bombs(
    s: &Scan,
    report: &mut ValidationReport,
    v: &mut Vec<Violation>,
    policy: &ValidationLimits,
) {
    report.function_count = s.imported_funcs.saturating_add(s.defined_funcs);
    report.custom_section_bytes = s.custom_section_bytes;

    let mut bomb = |cond: bool, detail: String| {
        if cond {
            v.push(Violation::new(ViolationCode::V4ResourceBomb, detail));
        }
    };
    bomb(
        report.function_count > policy.max_functions,
        format!(
            "{} functions exceeds MAX_FUNCTIONS={} — AOT compile time is superlinear in \
             function count and compile bombs are a real attack",
            report.function_count, policy.max_functions
        ),
    );
    bomb(
        s.max_locals > policy.max_locals_per_function,
        format!(
            "function #{} declares {} locals, exceeding MAX_LOCALS_PER_FUNCTION={}",
            s.max_locals_fn, s.max_locals, policy.max_locals_per_function
        ),
    );
    bomb(
        s.table_elements > policy.max_table_elements as u64,
        format!(
            "table space of {} elements exceeds MAX_TABLE_ELEMENTS={}",
            s.table_elements, policy.max_table_elements
        ),
    );
    bomb(
        s.custom_section_bytes > policy.max_custom_section_bytes,
        format!(
            "{} bytes of custom sections exceeds MAX_CUSTOM_SECTION_BYTES={} (largest: `{}`, \
             {} bytes)",
            s.custom_section_bytes,
            policy.max_custom_section_bytes,
            s.largest_custom.0,
            s.largest_custom.1
        ),
    );
    bomb(
        s.type_count > policy.max_types,
        format!(
            "{} types exceeds MAX_TYPES={}",
            s.type_count, policy.max_types
        ),
    );
    bomb(
        s.element_segments > policy.max_element_segments,
        format!(
            "{} element segments exceeds MAX_ELEMENT_SEGMENTS={}",
            s.element_segments, policy.max_element_segments
        ),
    );
    bomb(
        s.data_segments > policy.max_data_segments,
        format!(
            "{} data segments exceeds MAX_DATA_SEGMENTS={}",
            s.data_segments, policy.max_data_segments
        ),
    );
}

/// The V4 limits, published so tenants can design against them.
pub mod limits {
    pub const MAX_FUNCTIONS: u32 = isoplane_abi::config::ValidationLimits::DEFAULT.max_functions;
    pub const MAX_LOCALS_PER_FUNCTION: u32 =
        isoplane_abi::config::ValidationLimits::DEFAULT.max_locals_per_function;
    pub const MAX_TABLE_ELEMENTS: u32 =
        isoplane_abi::config::ValidationLimits::DEFAULT.max_table_elements;
    pub const MAX_CUSTOM_SECTION_BYTES: u64 =
        isoplane_abi::config::ValidationLimits::DEFAULT.max_custom_section_bytes;
    pub const MAX_IMPORTS: u32 = isoplane_abi::config::ValidationLimits::DEFAULT.max_imports;
    pub const MAX_EXPORTS: u32 = isoplane_abi::config::ValidationLimits::DEFAULT.max_exports;
    pub const MAX_TYPES: u32 = isoplane_abi::config::ValidationLimits::DEFAULT.max_types;
    pub const MAX_ELEMENT_SEGMENTS: u32 =
        isoplane_abi::config::ValidationLimits::DEFAULT.max_element_segments;
    pub const MAX_DATA_SEGMENTS: u32 =
        isoplane_abi::config::ValidationLimits::DEFAULT.max_data_segments;
}

#[cfg(test)]
mod tests {
    use isoplane_abi::config::Limits;

    use crate::validator::{validate, Violation, ViolationCode};

    fn guest(globals: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
  {globals}
  (memory (export "memory") 1 2)
  (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
  (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
  (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#
        ))
        .expect("the global fixture must assemble")
    }

    fn missing_global_exports(wasm: &[u8]) -> Vec<(ViolationCode, Option<String>)> {
        validate(wasm, Limits::default())
            .err()
            .unwrap_or_default()
            .iter()
            .map(Violation::parts)
            .filter(|(_, subject, _)| subject.is_some_and(|s| s.starts_with("global ")))
            .map(|(code, subject, _)| (code, subject.map(str::to_string)))
            .collect()
    }

    #[test]
    fn an_unexported_mutable_global_is_rejected_by_name() {
        let wasm = guest("(global $counter (mut i64) (i64.const 0))");
        let violations = validate(&wasm, Limits::default())
            .expect_err("an unexported mutable global must be rejected");
        assert_eq!(
            violations
                .iter()
                .map(|v| (v.code, v.subject.as_deref()))
                .collect::<Vec<_>>(),
            vec![(ViolationCode::V1MissingExport, Some("global 0 (`counter`)"))],
        );
        assert!(
            violations[0].detail.contains("only through their exports"),
            "the detail says why: {}",
            violations[0].detail
        );
    }

    #[test]
    fn an_exported_mutable_global_is_admitted() {
        let wasm = guest(r#"(global $counter (export "counter") (mut i64) (i64.const 0))"#);
        validate(&wasm, Limits::default()).expect("an exported mutable global is restorable");
    }

    #[test]
    fn an_unexported_immutable_global_is_admitted() {
        let wasm = guest("(global $limit i64 (i64.const 0))");
        validate(&wasm, Limits::default()).expect("an immutable global holds no state");
    }

    #[test]
    fn global_indices_count_imported_globals_first() {
        let wasm = guest(
            r#"(import "env" "base" (global i32))
  (global i32 (i32.const 0))
  (global (mut i32) (i32.const 0))"#,
        );
        assert_eq!(
            missing_global_exports(&wasm),
            vec![(ViolationCode::V1MissingExport, Some("global 2".to_string()))],
        );
    }
}
