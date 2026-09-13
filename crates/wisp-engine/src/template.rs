//! Templates: the compiled, pre-initialised unit a cell loads.

use std::sync::Arc;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use wasmtime::Module;

use crate::snapshot::GlobalValue;

/// Identity of a module and its immutable application content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModuleHash(pub [u8; 32]);

impl ModuleHash {
    pub fn of(bytes: &[u8]) -> Self {
        ModuleHash(*blake3::hash(bytes).as_bytes())
    }
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Display for ModuleHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.hex()[..16])
    }
}

/// Custom section pre-initialisation writes into the template wasm naming
/// the init units it left un-run. It travels in the artifact because the
/// deferred set is a property of one pre-initialisation, not of the module.
/// Layout: a little-endian `u32` count, then that many little-endian `u32`
/// unit ids. Absent means nothing deferred.
pub const DEFERRED_INIT_SECTION: &str = "wisp.deferred-init";

/// A compiled, pre-initialised module ready to be mapped into slots.
pub struct Template {
    pub hash: ModuleHash,
    /// A hibernation delta is valid only against its own module hash and ABI
    /// rev.
    pub abi_rev: u32,
    pub(crate) inner: Arc<TemplateInner>,
}

pub(crate) struct TemplateInner {
    pub application_files: crate::PlatformFiles,
    pub filesystem_files: crate::PlatformFiles,
    pub module: Module,
    /// A linker carrying this module's WASI PAL adapter, or `None` when the
    /// module imports only `wisp`. Per template because a trap stub is typed
    /// from the module's own declaration, and every instance shares it.
    pub pal_linker: Option<wasmtime::Linker<crate::state::StoreState>>,
    pub pal_composed: Vec<&'static wisp_abi::pal::PalDef>,
    /// The post-init linear memory, the baseline for the hibernation page
    /// diff. Its own `Arc` so the cell's activator holds it by refcount.
    pub image: Arc<crate::image::MemoryImage>,
    pub memory_pages: u32,
    pub deferred: Vec<u32>,
    /// Exported mutable globals in module index order, with their template
    /// values, so capture and restore agree on an ordering without names in
    /// the delta.
    pub mut_globals: Vec<(String, GlobalValue)>,
    /// Exported tables, in export order, with their instantiated sizes.
    pub tables: Vec<(String, u64)>,
    /// Per exported table, whether each slot held a non-null reference, so
    /// capture can detect a table mutation.
    pub table_nullness: Vec<Vec<bool>>,
    pub has_gc_hint: bool,
    pub has_initialize: bool,
}

impl Template {
    /// Bind immutable application files to an inactive template and its snapshot identity.
    /// Fails on host-path collisions or if the template is already shared or has application files.
    pub fn with_application_files(mut self, files: crate::PlatformFiles) -> Result<Self> {
        if files.files().is_empty() {
            return Ok(self);
        }
        let inner = Arc::get_mut(&mut self.inner)
            .ok_or_else(|| anyhow::anyhow!("cannot change files on a shared template"))?;
        anyhow::ensure!(
            inner.application_files.files().is_empty(),
            "template application files are already bound"
        );
        let combined = inner.filesystem_files.merge(&files)?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"wisp.application-template.v1\0");
        hash.update(&self.hash.0);
        hash.update(&postcard::to_allocvec(&files)?);
        self.hash = ModuleHash(*hash.finalize().as_bytes());
        inner.application_files = files;
        inner.filesystem_files = combined;
        Ok(self)
    }

    /// Whether the compiled module declares this import, including mapped artifacts.
    pub fn imports(&self, module: &str, name: &str) -> bool {
        self.inner
            .module
            .imports()
            .any(|import| import.module() == module && import.name() == name)
    }

    /// The pre-initialised linear memory, shared rather than copied.
    pub fn memory_image_arc(&self) -> Arc<crate::image::MemoryImage> {
        Arc::clone(&self.inner.image)
    }

    pub fn memory_pages(&self) -> u32 {
        self.inner.memory_pages
    }

    /// Init units left un-run because they touched a forbidden import in the
    /// init sandbox.
    pub fn deferred_init_units(&self) -> &[u32] {
        &self.inner.deferred
    }

    pub fn has_gc_hint(&self) -> bool {
        self.inner.has_gc_hint
    }

    pub fn has_initialize(&self) -> bool {
        self.inner.has_initialize
    }

    /// The `wasi:*` imports the PAL adapter composed for this template, so a
    /// capability that entered through a WASI door can be named afterwards.
    pub fn pal_composed(&self) -> &[&'static wisp_abi::pal::PalDef] {
        &self.inner.pal_composed
    }

    pub fn uses_pal(&self) -> bool {
        self.inner.pal_linker.is_some()
    }
}

impl std::fmt::Debug for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Template")
            .field("hash", &self.hash)
            .field("abi_rev", &self.abi_rev)
            .field("memory_pages", &self.inner.memory_pages)
            .field("deferred_init_units", &self.inner.deferred)
            .field("mutable_globals", &self.inner.mut_globals.len())
            .finish()
    }
}

pub(crate) fn read_deferred_units(wasm: &[u8]) -> Result<Vec<u32>> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::CustomSection(c) = payload? {
            if c.name() == DEFERRED_INIT_SECTION {
                let d = c.data();
                let Some(count) = d.get(0..4) else {
                    bail!("`{DEFERRED_INIT_SECTION}` section is truncated");
                };
                let n = u32::from_le_bytes(count.try_into().expect("a 4-byte slice")) as usize;
                let Some(ids) = d.get(4..4 + n * 4) else {
                    bail!(
                        "`{DEFERRED_INIT_SECTION}` declares {n} units but is {} bytes",
                        d.len()
                    );
                };
                return Ok(ids
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c))
                    .collect());
            }
        }
    }
    Ok(Vec::new())
}

/// Names of exported mutable globals, ordered by module global index. Only
/// exported globals are reachable through wasmtime's public API, so a mutable
/// global a guest does not export is invisible to hibernation.
pub(crate) fn exported_mutable_globals(wasm: &[u8]) -> Result<Vec<String>> {
    let mut mutability: Vec<bool> = Vec::new();
    let mut exports: Vec<(String, u32)> = Vec::new();

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            wasmparser::Payload::ImportSection(r) => {
                for imp in r.into_imports() {
                    if let wasmparser::TypeRef::Global(g) = imp?.ty {
                        mutability.push(g.mutable);
                    }
                }
            }
            wasmparser::Payload::GlobalSection(r) => {
                for g in r {
                    mutability.push(g?.ty.mutable);
                }
            }
            wasmparser::Payload::ExportSection(r) => {
                for e in r {
                    let e = e?;
                    if e.kind == wasmparser::ExternalKind::Global {
                        exports.push((e.name.to_string(), e.index));
                    }
                }
            }
            _ => {}
        }
    }

    exports.retain(|(_, idx)| mutability.get(*idx as usize).copied().unwrap_or(false));
    exports.sort_by_key(|(_, idx)| *idx);
    Ok(exports.into_iter().map(|(n, _)| n).collect())
}

/// Names of exported tables, in export order.
pub(crate) fn exported_tables(wasm: &[u8]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::ExportSection(r) = payload? {
            for e in r {
                let e = e?;
                if e.kind == wasmparser::ExternalKind::Table {
                    out.push(e.name.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// The cheap structural checks the validator also makes, repeated here so a
/// second linear memory or an undeclared import never reaches a slot, and so
/// the error names the offender rather than wasmtime's "unknown import".
pub(crate) fn structural_check(wasm: &[u8]) -> Result<()> {
    let mut memories = 0usize;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            wasmparser::Payload::MemorySection(r) => {
                for m in r {
                    let m = m?;
                    memories += 1;
                    if m.shared {
                        bail!("V2: shared memory is not permitted");
                    }
                    if m.memory64 {
                        bail!("V3: 64-bit linear memory is not permitted");
                    }
                }
            }
            wasmparser::Payload::ImportSection(r) => {
                for imp in r.into_imports() {
                    let imp = imp?;
                    if let wasmparser::TypeRef::Memory(_) = imp.ty {
                        memories += 1;
                    }
                    let declared = wisp_abi::imports::is_declared(imp.module, imp.name)
                        || wisp_abi::pal::lookup(imp.module, imp.name).is_some();
                    if !declared {
                        bail!(
                            "V1: undeclared import `{}::{}`; neither `wisp:platform` nor the \
                             WASI PAL whitelist declares it",
                            imp.module,
                            imp.name
                        );
                    }
                }
            }
            _ => {}
        }
    }
    if memories != 1 {
        bail!("V3: exactly one linear memory is required, found {memories}");
    }
    Ok(())
}

/// The metadata sidecar of a compiled artifact: what a cell must check before
/// it maps code, and what it needs beyond the `.cwasm` to finish a template.
#[derive(Serialize, Deserialize)]
pub(crate) struct ArtifactMeta {
    pub magic: [u8; 8],
    pub format: u32,
    pub abi_rev: u32,
    pub module_hash: [u8; 32],
    pub engine_fingerprint: String,
    /// blake3 of the `.cwasm`, checked before the file is mapped.
    pub cwasm_hash: [u8; 32],
    pub memory_pages: u32,
    pub deferred: Vec<u32>,
    pub mut_global_names: Vec<String>,
    pub table_names: Vec<String>,
}

pub(crate) const ARTIFACT_MAGIC: [u8; 8] = *b"WISPTMPL";
pub(crate) const ARTIFACT_FORMAT: u32 = 2;
