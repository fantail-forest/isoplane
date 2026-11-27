//! Exports a module's mutable globals and tables, so that what its
//! initializer leaves in them can be captured.

use anyhow::{bail, ensure};
use wasm_encoder::{ExportKind, Module, RawSection};
use wasmparser::{ExternalKind, Parser, Payload, ValType};

const GLOBAL_PREFIX: &str = "isoplane.prepare.global.";
const TABLE_PREFIX: &str = "isoplane.prepare.table.";

/// `wasm` itself when it has no initializer; otherwise `wasm` with every
/// mutable global and table exported. Fails if the module has a start
/// function, or state an initializer could change that a template cannot
/// carry: a mutable global that is not numeric, an imported global, table or
/// memory, or code that writes a table or drops a segment.
pub(crate) fn instrument(wasm: &[u8]) -> anyhow::Result<Vec<u8>> {
    if !has_initializer(wasm)? {
        return Ok(wasm.to_vec());
    }
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
    Ok(output.finish())
}

fn has_initializer(wasm: &[u8]) -> anyhow::Result<bool> {
    let mut found = false;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload? {
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    found |= export.name == isoplane_abi::imports::exports::INITIALIZE
                        && export.kind == ExternalKind::Func;
                }
            }
            Payload::StartSection { .. } => bail!(
                "preparation requires an explicit isoplane_initialize export, not a start section"
            ),
            _ => {}
        }
    }
    Ok(found)
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
