//! One pass over the wasm binary that every V1-V4 check reads from.

use std::collections::BTreeMap;

use isoplane_abi::imports::{Sig, Ty};
use wasmparser::{
    CompositeInnerType, ExternalKind, KnownCustom, MemoryType, Name, Parser, Payload, TypeRef,
    ValType,
};

#[derive(Default)]
pub(super) struct Scan {
    /// Function types by type index; `None` for non-function composite types.
    pub(super) func_types: Vec<Option<(Vec<ValType>, Vec<ValType>)>>,
    pub(super) imports: Vec<(String, String, TypeRef)>,
    pub(super) memories: Vec<MemoryType>,
    pub(super) exports: Vec<(String, ExternalKind, u32)>,
    /// Type index of every function in index-space order, imports first, so
    /// an export named by index can be type-checked.
    pub(super) func_type_of: Vec<u32>,
    /// Mutability of every global in index-space order, imports first.
    pub(super) global_mutable: Vec<bool>,
    /// Global names from the `name` section, by global index.
    pub(super) global_names: BTreeMap<u32, String>,
    pub(super) imported_funcs: u32,
    pub(super) defined_funcs: u32,
    pub(super) table_elements: u64,
    pub(super) max_locals: u32,
    pub(super) max_locals_fn: u32,
    pub(super) code_entries: u32,
    pub(super) custom_section_bytes: u64,
    pub(super) largest_custom: (String, u64),
    pub(super) type_count: u32,
    pub(super) element_segments: u32,
    pub(super) data_segments: u32,
    pub(super) nested: bool,
    pub(super) local_debug: bool,
}

pub(super) fn scan(wasm: &[u8]) -> anyhow::Result<Scan> {
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
                    match imp.ty {
                        TypeRef::Memory(m) => s.memories.push(m),
                        TypeRef::Global(g) => s.global_mutable.push(g.mutable),
                        _ => {}
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
            Payload::GlobalSection(r) => {
                for g in r {
                    s.global_mutable.push(g?.ty.mutable);
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
                s.local_debug |= c.name() == isoplane_abi::LOCAL_DEBUG_SECTION;
                if let KnownCustom::Name(names) = c.as_known() {
                    s.global_names = global_names(names);
                }
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

/// Engines ignore a malformed `name` section, and the names only label
/// violations, so a subsection that fails to parse contributes no names.
fn global_names(names: wasmparser::NameSectionReader<'_>) -> BTreeMap<u32, String> {
    names
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|name| match name {
            Name::Global(map) => Some(map),
            _ => None,
        })
        .flat_map(|map| map.into_iter().map_while(Result::ok))
        .map(|naming| (naming.index, naming.name.to_string()))
        .collect()
}

pub(super) fn func_type_index(t: &TypeRef) -> Option<u32> {
    match t {
        TypeRef::Func(i) | TypeRef::FuncExact(i) => Some(*i),
        _ => None,
    }
}

pub(super) fn kind_label(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::Memory(_) => "a memory",
        TypeRef::Table(_) => "a table",
        TypeRef::Global(_) => "a global",
        TypeRef::Tag(_) => "a tag",
        TypeRef::Func(_) | TypeRef::FuncExact(_) => "a function",
    }
}

pub(super) fn kind_matches(got: ExternalKind, want: ExternalKind) -> bool {
    got == want || (want == ExternalKind::Func && got == ExternalKind::FuncExact)
}

fn ty_matches(want: Ty, got: ValType) -> bool {
    matches!(
        (want, got),
        (Ty::I32, ValType::I32) | (Ty::I64, ValType::I64)
    )
}

pub(super) fn sig_matches(want: &Sig, params: &[ValType], results: &[ValType]) -> bool {
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

pub(super) fn render_sig(params: &[ValType], results: &[ValType]) -> String {
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

pub(super) fn render_want(sig: &Sig) -> String {
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
