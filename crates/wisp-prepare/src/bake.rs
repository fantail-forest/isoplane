//! Preserve initialized state in active segments so native compilation retains CoW.

use std::collections::BTreeMap;

use anyhow::{bail, ensure};
use wasm_encoder::{ConstExpr, ExportKind, Module, RawSection};
use wasmparser::{ExternalKind, Parser, Payload, ValType};
use wisp_engine::snapshot::GlobalValue;

const GLOBAL_PREFIX: &str = "wisp.prepare.global.";
const TABLE_PREFIX: &str = "wisp.prepare.table.";

pub(crate) struct Instrumented {
    pub wasm: Vec<u8>,
    pub globals: Vec<(u32, String)>,
}

pub(crate) fn instrument(wasm: &[u8]) -> anyhow::Result<Instrumented> {
    let mut globals = Vec::new();
    let mut tables = 0;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::GlobalSection(reader) => {
                for (index, global) in reader.into_iter().enumerate() {
                    let global = global?;
                    if global.ty.mutable {
                        ensure!(
                            matches!(
                                global.ty.content_type,
                                ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64
                            ),
                            "mutable global {index} has an unsupported capture type"
                        );
                        globals.push((u32::try_from(index)?, format!("{GLOBAL_PREFIX}{index}")));
                    }
                }
            }
            Payload::TableSection(reader) => tables = reader.count(),
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    ensure!(
                        matches!(import?.ty, wasmparser::TypeRef::Func(_)),
                        "preparation admits function imports only"
                    );
                }
            }
            Payload::StartSection { .. } => bail!(
                "preparation requires an explicit wisp_initialize export, not a start section"
            ),
            Payload::CodeSectionEntry(body) => {
                let mut reader = body.get_operators_reader()?;
                while !reader.eof() {
                    use wasmparser::Operator;
                    match reader.read()? {
                        Operator::TableSet { .. }
                        | Operator::TableGrow { .. }
                        | Operator::TableFill { .. }
                        | Operator::TableCopy { .. }
                        | Operator::TableInit { .. }
                        | Operator::ElemDrop { .. }
                        | Operator::DataDrop { .. } => bail!(
                            "preparation cannot preserve mutable table or passive segment state"
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut output = Module::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload?;
        if let Payload::ExportSection(reader) = payload {
            let mut exports = wasm_encoder::ExportSection::new();
            for export in reader {
                let export = export?;
                ensure!(
                    !export.name.starts_with(GLOBAL_PREFIX)
                        && !export.name.starts_with(TABLE_PREFIX),
                    "module collides with reserved preparation exports"
                );
                exports.export(export.name, export_kind(export.kind)?, export.index);
            }
            for (index, name) in &globals {
                exports.export(name, ExportKind::Global, *index);
            }
            for index in 0..tables {
                exports.export(&format!("{TABLE_PREFIX}{index}"), ExportKind::Table, index);
            }
            output.section(&exports);
        } else if let Some((id, range)) = payload.as_section() {
            output.section(&RawSection {
                id,
                data: &wasm[range],
            });
        }
    }
    Ok(Instrumented {
        wasm: output.finish(),
        globals,
    })
}

pub(crate) fn bake(
    wasm: &[u8],
    memory: &[u8],
    globals: &BTreeMap<u32, GlobalValue>,
    deferred: &[u32],
) -> anyhow::Result<Vec<u8>> {
    let data = baked_data(wasm, memory)?;
    let mut output = Module::new();
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload?;
        match payload {
            Payload::MemorySection(reader) => {
                let mut section = wasm_encoder::MemorySection::new();
                for ty in reader {
                    let ty = ty?;
                    section.memory(wasm_encoder::MemoryType {
                        minimum: u64::try_from(memory.len() / wisp_abi::WASM_PAGE_SIZE)?,
                        maximum: ty.maximum,
                        memory64: ty.memory64,
                        shared: ty.shared,
                        page_size_log2: ty.page_size_log2,
                    });
                }
                output.section(&section);
            }
            Payload::GlobalSection(reader) => {
                let mut section = wasm_encoder::GlobalSection::new();
                use wasm_encoder::reencode::Reencode;
                let mut encoder = wasm_encoder::reencode::RoundtripReencoder;
                for (index, global) in reader.into_iter().enumerate() {
                    let global = global?;
                    let value = match globals.get(&u32::try_from(index)?) {
                        Some(GlobalValue::I32(value)) => ConstExpr::i32_const(*value),
                        Some(GlobalValue::I64(value)) => ConstExpr::i64_const(*value),
                        Some(GlobalValue::F32(value)) => {
                            ConstExpr::f32_const(wasm_encoder::Ieee32::new(*value))
                        }
                        Some(GlobalValue::F64(value)) => {
                            ConstExpr::f64_const(wasm_encoder::Ieee64::new(*value))
                        }
                        None => {
                            ensure!(
                                !global.ty.mutable,
                                "initialized mutable global {index} was not captured"
                            );
                            encoder.const_expr(global.init_expr)?
                        }
                    };
                    section.global(encoder.global_type(global.ty)?, &value);
                }
                output.section(&section);
            }
            Payload::ExportSection(reader) => {
                let mut section = wasm_encoder::ExportSection::new();
                for export in reader {
                    let export = export?;
                    if export.name == wisp_abi::imports::exports::INITIALIZE {
                        continue;
                    }
                    section.export(export.name, export_kind(export.kind)?, export.index);
                }
                output.section(&section);
            }
            Payload::DataSection(_) => {}
            Payload::DataCountSection { .. } => {
                output.section(&wasm_encoder::DataCountSection { count: data.len() });
            }
            Payload::CustomSection(section)
                if section.name() == wisp_engine::DEFERRED_INIT_SECTION => {}
            other => {
                if let Some((id, range)) = other.as_section() {
                    output.section(&RawSection {
                        id,
                        data: &wasm[range],
                    });
                }
            }
        }
    }
    output.section(&data);
    let mut ids = Vec::new();
    ids.extend_from_slice(&u32::try_from(deferred.len())?.to_le_bytes());
    for id in deferred {
        ids.extend_from_slice(&id.to_le_bytes());
    }
    output.section(&wasm_encoder::CustomSection {
        name: wisp_engine::DEFERRED_INIT_SECTION.into(),
        data: ids.into(),
    });
    Ok(output.finish())
}

fn baked_data(wasm: &[u8], memory: &[u8]) -> anyhow::Result<wasm_encoder::DataSection> {
    let mut data = wasm_encoder::DataSection::new();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Payload::DataSection(reader) = payload? {
            for segment in reader {
                let segment = segment?;
                match segment.kind {
                    // Active segments are empty after instantiation. Retaining their indexes
                    // keeps memory.init's segment operands valid without rewriting guest code.
                    wasmparser::DataKind::Active { .. } => {
                        data.passive([]);
                    }
                    wasmparser::DataKind::Passive => {
                        data.passive(segment.data.iter().copied());
                    }
                }
            }
        }
    }
    let image = wisp_engine::MemoryImage::from_dense(memory);
    for (offset, bytes) in image.regions() {
        data.active(
            0,
            &ConstExpr::i32_const(u32::try_from(offset)? as i32),
            bytes.iter().copied(),
        );
    }
    Ok(data)
}

fn export_kind(kind: ExternalKind) -> anyhow::Result<ExportKind> {
    Ok(match kind {
        ExternalKind::Func | ExternalKind::FuncExact => ExportKind::Func,
        ExternalKind::Table => ExportKind::Table,
        ExternalKind::Memory => ExportKind::Memory,
        ExternalKind::Global => ExportKind::Global,
        ExternalKind::Tag => bail!("exception tags are not supported in prepared applications"),
    })
}
