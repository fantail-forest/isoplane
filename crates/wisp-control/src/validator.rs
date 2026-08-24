//! Deploy-time validation — **normative** (spec.md §4.3).
//!
//! This is where invariant 4 stops being an aspiration and becomes a checked
//! property: *"the import list is a complete, auditable capability manifest
//! checked at deploy time."* The four rules below are the whole of it, and
//! the first one is the one that matters — an undeclared import is an
//! attempted capability, and there is deliberately no dynamic-linking escape
//! hatch to work around it.
//!
//! Note the ordering argument in §4.5: because the validator refuses
//! threads/atomics (V2) and undeclared imports (V1), *blocking is not
//! expressible inside the sandbox at all*. It is not policed at runtime; it
//! is absent from the language the guest is allowed to speak.
//!
//! ## How V2 is implemented, and why that way
//!
//! §4.3 makes the argument itself: with the threads feature off,
//! `memory.atomic.wait*`/`notify` *fail bytecode validation*. So V2 is not a
//! bespoke opcode scanner — a scanner would be a second, weaker copy of the
//! engine's own rules and would drift from it. Instead [`ENGINE_FEATURES`]
//! declares the profile once, and V2 is "run `wasmparser`'s validator with
//! exactly the feature set the engine is configured with, and report what it
//! says." The validator and the engine then cannot disagree, because they are
//! reading the same constant.
//!
//! `wisp-engine` is expected to mirror [`ENGINE_FEATURES`] into its
//! `wasmtime::Config`; §8.2.2 wants the check to exist in both places, but
//! only one of them gets to define it.

use std::collections::BTreeSet;

use wasmparser::{
    CompositeInnerType, ExternalKind, MemoryType, Parser, Payload, TypeRef, ValType, Validator,
    WasmFeatures,
};
use wisp_abi::config::Limits;
use wisp_abi::imports::{self, ImportDef, ImportKind, Sig, Ty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationCode {
    /// **V1** — component types fail to match the declared world, or any
    /// import lies outside the world's allowlist.
    V1UndeclaredImport,
    /// **V1** — a required guest export is missing.
    V1MissingExport,
    /// **V1** — a declared import has the wrong signature.
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
}

impl Violation {
    fn new(code: ViolationCode, detail: impl Into<String>) -> Self {
        Violation {
            code,
            detail: detail.into(),
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
    /// The complete capability surface this module requested — the auditable
    /// manifest of invariant 4, and what a tenant sees in the deploy output.
    pub capabilities: Vec<&'static str>,
    pub memory_pages_max: Option<u32>,
    pub function_count: u32,
    pub module_bytes: u64,
    /// Declared initial size of the single linear memory, in 64 KiB pages.
    /// This is the size the template image starts at (§4.4).
    pub memory_pages_initial: Option<u32>,
    /// Total custom-section bytes (V4 heuristic input, and the thing that
    /// makes a 50 MB "module" that is 49 MB of debug info visible).
    pub custom_section_bytes: u64,
    /// Which optional exports are present (§5.2): `wisp_gc_hint` gates the
    /// pre-hibernation compaction hint, `wisp_initialize` gates pre-init.
    pub optional_exports: Vec<&'static str>,
}

impl ValidationReport {
    /// Does this module opt in to pre-initialization (§4.4)? A module without
    /// `wisp_initialize` is deployable — it simply has an empty template.
    pub fn has_initialize(&self) -> bool {
        self.optional_exports
            .contains(&imports::exports::INITIALIZE)
    }

    /// The tenant-facing capability manifest. This is deliberately rendered
    /// as a list of *powers granted*, not a list of symbols: invariant 4 says
    /// the import list IS the capability manifest, so it should read like one
    /// in the deploy output too.
    pub fn manifest(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "capability manifest ({} imports declared):",
            self.capabilities.len()
        );
        let mut groups: Vec<(&'static str, Vec<&'static ImportDef>)> = Vec::new();
        for name in &self.capabilities {
            let def = imports::lookup(name).expect("capabilities come from the table");
            let g = group_label(def);
            match groups.iter_mut().find(|(l, _)| *l == g) {
                Some((_, v)) => v.push(def),
                None => groups.push((g, vec![def])),
            }
        }
        for (label, defs) in &groups {
            let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
            let _ = writeln!(s, "  {label:<14} {}", names.join(", "));
        }
        // The absences are the interesting half of a capability manifest.
        let denied: Vec<&str> = imports::IMPORTS
            .iter()
            .filter(|d| !self.capabilities.contains(&d.name))
            .map(|d| d.name)
            .collect();
        let _ = writeln!(
            s,
            "  {:<14} {} imports NOT requested and therefore unreachable",
            "denied",
            denied.len()
        );
        if let Some(max) = self.memory_pages_max {
            let _ = writeln!(
                s,
                "  {:<14} 1 linear memory, {} .. {} pages ({} MiB max)",
                "memory",
                self.memory_pages_initial.unwrap_or(0),
                max,
                (max as u64 * 65_536) >> 20
            );
        }
        s
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

/// The engine profile: **one** declaration of which WebAssembly proposals the
/// platform speaks, consumed by V2 here and mirrored into the engine's
/// `wasmtime::Config` (§8.2.2, defense in depth — same rule, two places, one
/// definition).
///
/// Every subtraction below is load-bearing:
///
/// * `threads` / `shared_everything_threads` — §4.3 V2 and §8.4. No threads
///   means no shared memory, no atomics, and therefore **no counting thread**:
///   the measurement primitive most Spectre-class attacks need is not merely
///   discouraged, it fails to validate.
/// * `relaxed_simd` — §4.3 V2: deterministic SIMD subset only. Relaxed SIMD's
///   results are host-dependent, which would make `wisp replay` (§10) a lie.
/// * `exceptions` / `legacy_exceptions` — §4.3 V2, "beyond the profile".
/// * `stack_switching` — §5.1: the ABI is callback-CPS, with no engine stack
///   suspension in v1. Engine-suspended stacks are exactly the thing
///   invariant 3 exists to forbid, so the proposal that reintroduces them is
///   off at the bytecode level.
/// * `tail_call` — §4.3 V2 names "tail-calls-to-imports tricks outside the
///   world". Conservative reading: the proposal is off. If a real .NET
///   NativeAOT-LLVM module needs it, this is the one line to revisit — and
///   the V1 allowlist still covers the "outside the world" half.
/// * `multi_memory` / `memory64` — §5.5, "exactly one linear memory", 32-bit.
///   V3 says the same thing structurally; both are checked.
/// * `gc` / `gc_types` — the guest ships its own GC inside linear memory
///   (§4.2), so engine GC types would be a second heap the platform cannot
///   see. Invariant 1 says all tenant state is visible in linear memory +
///   globals + tables; wasm-GC objects would break that. Turning `gc_types`
///   off also removes `externref`, which is the right answer for a different
///   reason: §5.5 says no host pointers ever enter guest memory, and every
///   host resource in this platform is an opaque `u32` handle instead.
/// * `component_model` — the documented PoC deviation: the same world,
///   lowered to a flat core-wasm ABI under module `wisp`.
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

/// §4.3. Rejects on ANY violation; a module is deployable only if all four
/// rules hold. Runs inside the resource-capped, network-less sandbox of §8.7.
pub fn validate(
    wasm: &[u8],
    budgets: Limits,
) -> std::result::Result<ValidationReport, Vec<Violation>> {
    let mut v = Vec::new();
    let mut report = ValidationReport {
        module_bytes: wasm.len() as u64,
        ..Default::default()
    };

    // --- structural pass: V1 (shape), V3, V4 ------------------------------
    // Done with the streaming parser rather than the validator so that a
    // module which fails V2 still produces a complete, useful report: a
    // tenant should learn everything wrong with an artifact in one round
    // trip, not one violation per deploy attempt.
    // A component is not a core module, and the difference is visible in the
    // 8-byte header — worth checking before the parser, so the rejection can
    // say *why* rather than "unexpected section" (§4.3 V1: component types
    // must match the declared world; the PoC's world is lowered to core wasm).
    if is_component(wasm) {
        return Err(vec![Violation::new(
            ViolationCode::V1UndeclaredImport,
            "artifact is a WebAssembly *component*; this platform build lowers \
             wisp:platform@1 to a flat core-wasm ABI under module `wisp` (documented \
             deviation) and links no component imports",
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

    // --- V2: the engine's own bytecode validation, with our feature set ---
    if let Err(e) = Validator::new_with_features(ENGINE_FEATURES).validate_all(wasm) {
        // Distinguishing "uses a proposal we turned off" from "is simply
        // malformed" costs one more parse and makes the rejection message
        // actionable, which matters because V2 rejections are the ones a
        // tenant is most likely to think are our bug.
        let malformed = Validator::new_with_features(WasmFeatures::all())
            .validate_all(wasm)
            .is_err();
        let detail = if malformed {
            format!("bytecode validation failed: {}", e.message())
        } else {
            format!(
                "requires a WebAssembly proposal disabled by the WISP engine profile (§4.3 V2): {}",
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

/// Core module header is `\0asm 01 00 00 00`; a component's layer field
/// (bytes 6..8) is `01 00`.
fn is_component(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[0..4] == [0x00, 0x61, 0x73, 0x6d] && wasm[6..8] == [0x01, 0x00]
}

// ---------------------------------------------------------------------------
// structural scan
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Scan {
    /// Function types by type index; `None` for non-function composite types.
    func_types: Vec<Option<(Vec<ValType>, Vec<ValType>)>>,
    imports: Vec<(String, String, TypeRef)>,
    memories: Vec<MemoryType>,
    exports: Vec<(String, ExternalKind)>,
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
    /// A core module that contains nested modules/components is not the
    /// artifact shape the PoC accepts (§4.1 uploads one module).
    nested: bool,
}

fn scan(wasm: &[u8]) -> anyhow::Result<Scan> {
    let mut s = Scan::default();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::Version { encoding, .. } => {
                if encoding != wasmparser::Encoding::Module {
                    anyhow::bail!(
                        "expected a core wasm module; got a component. The PoC lowers \
                         wisp:platform@1 to a flat core-wasm ABI (documented deviation)"
                    );
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
                for imp in r {
                    let imp = imp?;
                    if let TypeRef::Func(_) = imp.ty {
                        s.imported_funcs += 1;
                    }
                    if let TypeRef::Memory(m) = imp.ty {
                        // An imported memory is still a memory for V3.
                        s.memories.push(m);
                    }
                    s.imports
                        .push((imp.module.to_string(), imp.name.to_string(), imp.ty));
                }
            }
            Payload::FunctionSection(r) => s.defined_funcs = r.count(),
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
                    s.exports.push((e.name.to_string(), e.kind));
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

// ---------------------------------------------------------------------------
// V1 — the capability surface
// ---------------------------------------------------------------------------

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
             core module and nested instantiation is a linking escape hatch (§4.3 V1)",
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
        // The whole of invariant 4 lives in this branch: there is no fallback
        // path, no "unknown module is linked dynamically", no host-provided
        // libc. If it is not in the table, the module does not deploy.
        let Some(def) = (if module == imports::HOST_MODULE {
            imports::lookup(field)
        } else {
            None
        }) else {
            v.push(Violation::new(
                ViolationCode::V1UndeclaredImport,
                format!(
                    "import `{module}`.`{field}` is not in the wisp:platform@1 capability table — \
                     an undeclared import is an attempted capability (§4.3 V1)"
                ),
            ));
            continue;
        };

        match ty {
            TypeRef::Func(idx) => match s.func_types.get(*idx as usize) {
                Some(Some((params, results))) => {
                    if sig_matches(&def.sig, params, results) {
                        seen.insert(def.name);
                    } else {
                        // Name-only checking would be theatre: a module could
                        // declare `kv_get` with a different shape and reach
                        // host memory through a type confusion. The engine
                        // would refuse to link it, but the *deploy* is where
                        // a tenant is entitled to hear about it.
                        v.push(Violation::new(
                            ViolationCode::V1SignatureMismatch,
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
            other => v.push(Violation::new(
                ViolationCode::V1SignatureMismatch,
                format!(
                    "import `wisp`.`{}` is declared as {} — every entry in the world is a \
                     function; importing a memory/table/global from the host would put a \
                     host object inside the sandbox (§5.5: no host pointers ever enter \
                     guest memory)",
                    def.name,
                    match other {
                        TypeRef::Memory(_) => "a memory",
                        TypeRef::Table(_) => "a table",
                        TypeRef::Global(_) => "a global",
                        TypeRef::Tag(_) => "a tag",
                        TypeRef::Func(_) => unreachable!(),
                    }
                ),
            )),
        }
    }

    // Report in table order, not module order: the manifest is the platform's
    // document, not the tenant's.
    report.capabilities = imports::IMPORTS
        .iter()
        .map(|d| d.name)
        .filter(|n| seen.contains(n))
        .collect();
}

fn check_exports(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    for required in imports::exports::REQUIRED {
        let want_kind = if *required == imports::exports::MEMORY {
            ExternalKind::Memory
        } else {
            ExternalKind::Func
        };
        match s.exports.iter().find(|(n, _)| n == required) {
            Some((_, k)) if *k == want_kind => {}
            Some((_, k)) => v.push(Violation::new(
                ViolationCode::V1MissingExport,
                format!("export `{required}` is a {k:?}, expected {want_kind:?} (§5.2)"),
            )),
            None => v.push(Violation::new(
                ViolationCode::V1MissingExport,
                format!(
                    "required guest export `{required}` is missing — the host has no way to \
                     enter this module (§5.2)"
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
                .any(|(n, k)| n == o && *k == ExternalKind::Func)
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

// ---------------------------------------------------------------------------
// V3 — memory shape
// ---------------------------------------------------------------------------

fn check_memory(s: &Scan, budgets: Limits, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    let ceiling_bytes = budgets.instance_memory_max;
    let ceiling_pages = ceiling_bytes / wisp_abi::WASM_PAGE_SIZE as u64;

    if s.memories.len() != 1 {
        v.push(Violation::new(
            ViolationCode::V3MemoryShape,
            format!(
                "module declares {} linear memories; exactly one is permitted (§5.5). \
                 One memory is what makes the template image, the CoW clone and the \
                 hibernation page-diff a single object rather than a set",
                s.memories.len()
            ),
        ));
    }

    for m in &s.memories {
        if m.shared {
            // A shared memory is the *representation* of threads. It is
            // refused here as well as by V2 because §4.3 lists it as its own
            // rule: even if some future engine profile enabled atomics for
            // single-threaded use, sharing would still be forbidden.
            v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                "linear memory is declared `shared` — threads and shared memory are off \
                 (§4.3 V2/V3, §8.4: no shared memory means no counting thread)",
            ));
        }
        if m.memory64 {
            v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                "64-bit linear memory is outside the engine profile (§5.5)",
            ));
        }
        match m.maximum {
            Some(max) if max > ceiling_pages => v.push(Violation::new(
                ViolationCode::V3MemoryShape,
                format!(
                    "declared memory maximum {} pages ({} MiB) exceeds the application memory \
                     ceiling of {} pages ({} MiB) (§11.1)",
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
                     pages — the module could never be instantiated (§11.1)",
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

// ---------------------------------------------------------------------------
// V4 — structural resource bombs
// ---------------------------------------------------------------------------

fn check_bombs(s: &Scan, report: &mut ValidationReport, v: &mut Vec<Violation>) {
    report.function_count = s.imported_funcs.saturating_add(s.defined_funcs);
    report.custom_section_bytes = s.custom_section_bytes;

    // These are compile bombs, not runtime bombs: §8.7 treats Cranelift
    // compilation of tenant bytes as parsing hostile input, and the cheapest
    // defense against a pathological compile is to not start it.
    let mut bomb = |cond: bool, detail: String| {
        if cond {
            v.push(Violation::new(ViolationCode::V4ResourceBomb, detail));
        }
    };
    bomb(
        report.function_count > limits::MAX_FUNCTIONS,
        format!(
            "{} functions exceeds MAX_FUNCTIONS={} — AOT compile time is superlinear in \
             function count and compile bombs are a real attack (§8.7)",
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
             {} bytes) — custom sections are not code, so this is storage and bandwidth \
             the tenant is asking the platform to replicate to every PoP (§9.3)",
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

/// Which capability groups a validated module actually holds — used by the
/// registry to record what a version was permitted to do at the time it was
/// sealed, so a later audit does not have to re-derive it from bytes.
pub fn capability_kinds(report: &ValidationReport) -> Vec<(&'static str, &'static str)> {
    report
        .capabilities
        .iter()
        .filter_map(|n| imports::lookup(n))
        .map(|d| {
            (
                d.name,
                match d.kind {
                    ImportKind::Sync => "sync",
                    ImportKind::Future => "future",
                },
            )
        })
        .collect()
}

/// §4.4 init limits: "Init MUST complete within 10 s CPU / 512 MB."
pub const INIT_CPU_LIMIT_MS: u64 = 10_000;
pub const INIT_MEMORY_LIMIT_BYTES: u64 = 512 << 20;

/// §4.3 V4 published limits. Real numbers so tenants can design against them.
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
