//! Deploy-time validation. The import list is the capability manifest (V1),
//! and with threads and atomics outside the engine profile (V2) blocking is
//! not expressible inside the sandbox at all. V2 is `wasmparser`'s validator
//! run with [`ENGINE_FEATURES`], the one declaration of the profile the
//! engine is configured from, so the two cannot disagree.

use std::collections::BTreeSet;

use wasmparser::{
    CompositeInnerType, ExternalKind, MemoryType, Parser, Payload, TypeRef, ValType, Validator,
    WasmFeatures,
};
use wisp_abi::config::Limits;
use wisp_abi::imports::{self, ImportDef, Sig, Ty};

use wisp_abi::pal::{self, Disposition, PalDef};

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
    /// memory maximum exceeding the application memory ceiling.
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
    fn new(code: ViolationCode, detail: impl Into<String>) -> Self {
        Violation {
            code,
            detail: detail.into(),
            subject: None,
        }
    }
    fn about(code: ViolationCode, subject: impl Into<String>, detail: impl Into<String>) -> Self {
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
        Ctx => "ctx",
    }
}

/// The WebAssembly proposals the platform speaks, enforced here as V2 and
/// configured into the engine from this one definition.
///
/// * `threads` / `shared_everything_threads` — V2. No threads means no shared
///   memory, no atomics, and therefore **no counting thread**: the measurement
///   primitive most Spectre-class attacks need is not merely discouraged, it
///   fails to validate.
/// * `relaxed_simd` — V2: use the SIMD subset without implementation-dependent
///   result choices.
/// * `exceptions` / `legacy_exceptions` — V2, beyond the profile.
/// * `stack_switching` — the ABI is callback-CPS, with no engine stack
///   suspension. Engine-suspended stacks are exactly the thing invariant 3
///   exists to forbid, so the proposal that reintroduces them is off at the
///   bytecode level.
/// * `tail_call` — off as a conservative reading; a tail call to an import
///   is still covered by V1.
/// * `multi_memory` / `memory64` — exactly one linear memory, 32-bit. V3 says
///   the same thing structurally; both are checked.
/// * `gc` / `gc_types` — the guest ships its own GC inside linear memory, so
///   engine GC types would be a second heap the platform cannot see.
///   Invariant 1 says all tenant state is visible in linear memory + globals +
///   tables; wasm-GC objects would break that. Turning `gc_types` off also
///   removes `externref`, which is the right answer for a different reason: no
///   host pointers ever enter guest memory, and every host resource in this
///   platform is an opaque `u32` handle instead.
/// * `component_model` — the world is lowered to a flat core-wasm ABI under
///   module `wisp`.
pub const ENGINE_FEATURES: WasmFeatures = {
    let f = WasmFeatures::WASM2
        .union(WasmFeatures::EXTENDED_CONST)
        .union(WasmFeatures::MULTI_VALUE)
        .union(WasmFeatures::SIGN_EXTENSION)
        .union(WasmFeatures::SATURATING_FLOAT_TO_INT)
        .union(WasmFeatures::BULK_MEMORY)
        .union(WasmFeatures::REFERENCE_TYPES)
        .union(WasmFeatures::SIMD);
    f.difference(
        WasmFeatures::THREADS
            .union(WasmFeatures::SHARED_EVERYTHING_THREADS)
            .union(WasmFeatures::RELAXED_SIMD)
            .union(WasmFeatures::EXCEPTIONS)
            .union(WasmFeatures::LEGACY_EXCEPTIONS)
            .union(WasmFeatures::STACK_SWITCHING)
            .union(WasmFeatures::TAIL_CALL)
            .union(WasmFeatures::MULTI_MEMORY)
            .union(WasmFeatures::MEMORY64)
            .union(WasmFeatures::GC)
            .union(WasmFeatures::GC_TYPES)
            .union(WasmFeatures::CUSTOM_PAGE_SIZES)
            .union(WasmFeatures::MEMORY_CONTROL)
            .union(WasmFeatures::WIDE_ARITHMETIC)
            .union(WasmFeatures::COMPONENT_MODEL),
    )
};

/// Every violation, not the first: a tenant learns everything wrong with an
/// artifact in one round trip.
pub fn validate(
    wasm: &[u8],
    budgets: Limits,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    let mut v = Vec::new();
    let mut report = ValidationReport {
        module_bytes: wasm.len() as u64,
        ..Default::default()
    };

    // The header says component or module, and the rejection can say which.
    if is_component(wasm) {
        return Err(vec![Violation::new(
            ViolationCode::V1UndeclaredImport,
            "artifact is a WebAssembly component; this platform lowers wisp:platform@1 to a \
             flat core-wasm ABI under module `wisp` and links no component imports",
        )]);
    }

    match scan(wasm) {
        Ok(scan) => {
            check_imports(&scan, &mut report, &mut v);
            check_exports(&scan, &mut report, &mut v);
            check_memory(&scan, budgets, &mut report, &mut v);
            check_bombs(&scan, &mut report, &mut v);
        }
        Err(e) => {
            v.push(Violation::new(
                ViolationCode::V2ForbiddenFeature,
                format!("binary is not a well-formed core module: {e}"),
            ));
        }
    }

    // The structural scan runs first so a module that fails V2 still gets a
    // complete report; a second parse with every feature on tells "uses a
    // proposal the profile turns off" from "malformed".
    if let Err(e) = Validator::new_with_features(ENGINE_FEATURES).validate_all(wasm) {
        let malformed = Validator::new_with_features(WasmFeatures::all())
            .validate_all(wasm)
            .is_err();
        let detail = if malformed {
            format!("bytecode validation failed: {}", e.message())
        } else {
            format!(
                "requires a WebAssembly proposal disabled by the WISP engine profile (V2): {}",
                e.message()
            )
        };
        v.push(Violation::new(ViolationCode::V2ForbiddenFeature, detail));
    }

    if v.is_empty() {
        Ok(report)
    } else {
        Err(v)
    }
}

/// A component's layer field (bytes 6..8) is `01 00`; a core module's is
/// `00 00`.
fn is_component(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[0..4] == [0x00, 0x61, 0x73, 0x6d] && wasm[6..8] == [0x01, 0x00]
}

#[derive(Default)]
struct Scan {
    /// Function types by type index; `None` for non-function composite types.
    func_types: Vec<Option<(Vec<ValType>, Vec<ValType>)>>,
    imports: Vec<(String, String, TypeRef)>,
    memories: Vec<MemoryType>,
    exports: Vec<(String, ExternalKind, u32)>,
    /// Type index of every function in index-space order, imports first, so
    /// an export named by index can be type-checked.
    func_type_of: Vec<u32>,
    imported_funcs: u32,
    defined_funcs: u32,
    table_elements: u64,
    max_locals: u32,
    max_locals_fn: u32,
    code_entries: u32,
    custom_section_bytes: u64,
    largest_custom: (String, u64),
    type_count: u32,
    element_segments: u32,
    data_segments: u32,
    nested: bool,
}

fn scan(wasm: &[u8]) -> anyhow::Result<Scan> {
    let mut s = Scan::default();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::Version { encoding, .. } => {
                if encoding != wasmparser::Encoding::Module {
                    anyhow::bail!("expected a core wasm module; got a component");
                }
            }
            Payload::TypeSection(r) => {
                for group in r {
                    for sub in group?.into_types() {
                        s.type_count += 1;
                        match sub.composite_type.inner {
                            CompositeInnerType::Func(ft) => s
                                .func_types
                                .push(Some((ft.params().to_vec(), ft.results().to_vec()))),
                            _ => s.func_types.push(None),
                        }
                    }
                }
            }
            Payload::ImportSection(r) => {
                // The compact-import encodings are outside the profile, and
                // still scanned: an undeclared import must not hide behind an
                // encoding V2 rejects second.
                for imp in r.into_imports() {
                    let imp = imp?;
                    if let Some(t) = func_type_index(&imp.ty) {
                        s.imported_funcs += 1;
                        s.func_type_of.push(t);
                    }
                    if let TypeRef::Memory(m) = imp.ty {
                        s.memories.push(m);
                    }
                    s.imports
                        .push((imp.module.to_string(), imp.name.to_string(), imp.ty));
                }
            }
            Payload::FunctionSection(r) => {
                s.defined_funcs = r.count();
                for t in r {
                    s.func_type_of.push(t?);
                }
            }
            Payload::MemorySection(r) => {
                for m in r {
                    s.memories.push(m?);
                }
            }
            Payload::TableSection(r) => {
                for t in r {
                    let t = t?;
                    s.table_elements += t.ty.maximum.unwrap_or(t.ty.initial);
                }
            }
            Payload::ExportSection(r) => {
                for e in r {
                    let e = e?;
                    s.exports.push((e.name.to_string(), e.kind, e.index));
                }
            }
            Payload::ElementSection(r) => s.element_segments = r.count(),
            Payload::DataSection(r) => s.data_segments = r.count(),
            Payload::CodeSectionEntry(body) => {
                let mut locals = 0u32;
                for l in body.get_locals_reader()? {
                    let (count, _ty) = l?;
                    locals = locals.saturating_add(count);
                }
                if locals > s.max_locals {
                    s.max_locals = locals;
                    s.max_locals_fn = s.imported_funcs + s.code_entries;
                }
                s.code_entries += 1;
            }
            Payload::CustomSection(c) => {
                s.custom_section_bytes += c.data().len() as u64;
                if c.data().len() as u64 > s.largest_custom.1 {
                    s.largest_custom = (c.name().to_string(), c.data().len() as u64);
                }
            }
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => s.nested = true,
            _ => {}
        }
    }
    Ok(s)
}

fn func_type_index(t: &TypeRef) -> Option<u32> {
    match t {
        TypeRef::Func(i) | TypeRef::FuncExact(i) => Some(*i),
        _ => None,
    }
}

fn kind_label(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::Memory(_) => "a memory",
        TypeRef::Table(_) => "a table",
        TypeRef::Global(_) => "a global",
        TypeRef::Tag(_) => "a tag",
        TypeRef::Func(_) | TypeRef::FuncExact(_) => "a function",
    }
}

fn kind_matches(got: ExternalKind, want: ExternalKind) -> bool {
    got == want || (want == ExternalKind::Func && got == ExternalKind::FuncExact)
}

fn ty_matches(want: Ty, got: ValType) -> bool {
    matches!(
        (want, got),
        (Ty::I32, ValType::I32) | (Ty::I64, ValType::I64)
    )
}

fn sig_matches(want: &Sig, params: &[ValType], results: &[ValType]) -> bool {
    want.params.len() == params.len()
        && want.results.len() == results.len()
        && want
            .params
            .iter()
            .zip(params)
            .all(|(a, b)| ty_matches(*a, *b))
        && want
            .results
            .iter()
            .zip(results)
            .all(|(a, b)| ty_matches(*a, *b))
}

fn render_sig(params: &[ValType], results: &[ValType]) -> String {
    let f = |v: &[ValType]| {
        v.iter()
            .map(|t| match t {
                ValType::I32 => "i32",
                ValType::I64 => "i64",
                ValType::F32 => "f32",
                ValType::F64 => "f64",
                ValType::V128 => "v128",
                ValType::Ref(_) => "ref",
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("({}) -> ({})", f(params), f(results))
}

fn render_want(sig: &Sig) -> String {
    let f = |v: &[Ty]| {
        v.iter()
            .map(|t| match t {
                Ty::I32 => "i32",
                Ty::I64 => "i64",
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("({}) -> ({})", f(sig.params), f(sig.results))
}

fn check_imports(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
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

fn check_exports(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
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

fn check_memory(s: &Scan, budgets: Limits, report: &mut ValidationReport, v: &mut Vec<Violation>) {
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

fn check_bombs(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
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
