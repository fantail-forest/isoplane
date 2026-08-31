//! Templates (spec.md §4.4) — the unit the data plane loads.
//!
//! > *The template, not the raw module, is the unit the data plane loads.
//! > Cold activation therefore skips: assembly load (none), JIT (none),
//! > static init (already ran), config parse (already ran).*

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use wasmtime::Module;

use crate::snapshot::GlobalValue;

/// Content address of the tenant's uploaded module.
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

/// Custom section `wisp-control`'s pre-init writes into the template wasm to
/// record which init units it had to leave un-run (§4.4 impure-cctor deferral).
///
/// It has to travel *in the artifact*: `deferred_init_units` is a property of
/// this particular pre-initialization, not of the module, and the data plane
/// only ever sees the template. Layout is a little-endian `u32` count followed
/// by that many little-endian `u32` unit ids. Absent means nothing deferred.
pub const DEFERRED_INIT_SECTION: &str = "wisp.deferred-init";

/// A compiled, pre-initialized module ready to be cloned into slots.
///
/// The memory image behind this is page-aligned and memfd-backed, so
/// activation is `mmap MAP_PRIVATE` — O(page tables), no copy — and every
/// instance of the module on the machine shares its pages until written
/// (§6.2). Concretely, that mapping is wasmtime's `memory_init_cow` image,
/// which is what `Config::memory_init_cow(true)` and the pooling allocator buy
/// us; §6.2 says the wasmtime instance-pooling design is "adopted wholesale"
/// and this is where that happens.
pub struct Template {
    pub hash: ModuleHash,
    /// Bound into every hibernation delta (§6.6): a delta is only valid
    /// against `(module hash, engine ABI rev)`.
    pub abi_rev: u32,
    pub(crate) inner: Arc<TemplateInner>,
}

pub(crate) struct TemplateInner {
    pub module: Module,
    /// §4.3: a linker carrying this module's **WASI PAL adapter**, or `None`
    /// when the module imports only `wisp` — which is every guest the Rust SDK
    /// builds, so the common path costs exactly nothing.
    ///
    /// Per *template* rather than per engine or per activation, and both ends
    /// of that matter. Per engine is impossible: a `TrapOnCall` stub is
    /// generated from the type the module itself declares (`PalDef::sig` is
    /// deliberately `None` for those), so it cannot be built without a module
    /// in hand. Per activation would put a ~45-entry map clone on the §6.5
    /// "activation p50 ≤ 1 ms" path to save nothing — templates are shared by
    /// every instance of a module on the machine (§4.4).
    pub pal_linker: Option<wasmtime::Linker<crate::state::StoreState>>,
    /// What the adapter composed, for the §4.3 audit trail.
    pub pal_composed: Vec<&'static wisp_abi::pal::PalDef>,
    /// The post-init linear memory as laid down at deploy time. This is the
    /// baseline the hibernation page-diff is taken against (§6.6) and the
    /// oracle for "reset restores template contents".
    ///
    /// Stored as its non-zero runs, not as the memory. A pre-initialized .NET
    /// template declares 53,084,160 bytes of which 147,207 are content; a
    /// `Vec<u8>` here charged every cell 53 MB per template for that, and
    /// `wisp-host`'s activator then copied it again. See `image::MemoryImage`.
    ///
    /// Its own `Arc` rather than a plain field, so `wisp-host`'s activator can
    /// hold the baseline for §6.6 by bumping a refcount instead of taking the
    /// second copy it used to take.
    pub image: Arc<crate::image::MemoryImage>,
    pub memory_pages: u32,
    pub deferred: Vec<u32>,
    /// Exported mutable globals in module-global-index order, with the values
    /// they hold in the template. §6.2 step 2 ("initialize mutable globals from
    /// the template's sealed values") is performed by wasmtime instantiation
    /// itself; these are kept so the seal can be *verified* and so
    /// capture/restore agree on an ordering without carrying names in the
    /// delta.
    pub mut_globals: Vec<(String, GlobalValue)>,
    /// Exported tables, in export order, with their instantiated sizes.
    pub tables: Vec<(String, u64)>,
    /// Per exported table, whether each slot held a non-null reference in the
    /// template. Recorded once per template (not per activation) so that
    /// capture can *detect* a table mutation cheaply. See the documented gap in
    /// `Instance::capture_mutable_state`: detection is possible, representation
    /// is not, so the engine fails loudly rather than losing half of invariant 1.
    pub table_nullness: Vec<Vec<bool>>,
    pub has_gc_hint: bool,
    pub has_initialize: bool,
}

impl Template {
    /// The pre-initialized linear memory, as laid down at deploy time. This
    /// is the baseline the hibernation page-diff is taken against.
    pub fn memory_image(&self) -> &crate::image::MemoryImage {
        &self.inner.image
    }

    /// The image as an `Arc` a consumer can hold without copying it.
    ///
    /// `wisp-host`'s activator needs the baseline on an execution-pool thread
    /// for §6.6's page-diff. It used to take `memory_image().to_vec()`, which
    /// for a .NET template was a second 53 MB allocation per module per cell;
    /// the template is already an `Arc`, so sharing it costs a refcount.
    pub fn memory_image_arc(&self) -> Arc<crate::image::MemoryImage> {
        Arc::clone(&self.inner.image)
    }

    pub fn memory_pages(&self) -> u32 {
        self.inner.memory_pages
    }

    /// Which init units were left un-run because they touched a forbidden
    /// import in the sealed sandbox (§4.4 impure-cctor deferral). They run
    /// lazily, per instance, on first touch.
    pub fn deferred_init_units(&self) -> &[u32] {
        &self.inner.deferred
    }

    /// §5.2: `wisp_gc_hint` is optional but strongly recommended — a guest
    /// that does not export it is correct, but stores a larger hibernation
    /// delta and pays for it (§5.5, §11.2).
    pub fn has_gc_hint(&self) -> bool {
        self.inner.has_gc_hint
    }

    /// §4.4: whether the module can be driven through the sealed init sandbox.
    pub fn has_initialize(&self) -> bool {
        self.inner.has_initialize
    }

    /// §4.3: the `wasi:*` imports the PAL adapter composed for this template,
    /// empty for a module that declares none.
    ///
    /// This is the audit trail the no-new-channels rule needs to be
    /// *checkable*: a capability that entered through a WASI door has to be
    /// nameable afterwards, or "no new channels" is a promise nobody can
    /// verify. The two that can carry a real capability —
    /// `monotonic-clock::now` and `random::get-random-bytes` — resolve to
    /// `clock_now_coarse` and `entropy_fill`, which are already in the
    /// tenant-facing manifest.
    pub fn pal_composed(&self) -> &[&'static wisp_abi::pal::PalDef] {
        &self.inner.pal_composed
    }

    /// Whether this template needed the WASI PAL adapter at all.
    pub fn uses_pal(&self) -> bool {
        self.inner.pal_linker.is_some()
    }

    /// Names of the exported mutable globals, in the order `InstanceImage`
    /// captures them. Exposed so `wisp-hibernate` can sanity-check a delta
    /// against the template it claims to be bound to.
    pub fn mutable_global_names(&self) -> impl Iterator<Item = &str> {
        self.inner.mut_globals.iter().map(|(n, _)| n.as_str())
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

/// Parse the deferred-unit custom section out of a template module.
pub(crate) fn read_deferred_units(wasm: &[u8]) -> Result<Vec<u32>> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::CustomSection(c) = payload? {
            if c.name() == DEFERRED_INIT_SECTION {
                let d = c.data();
                if d.len() < 4 {
                    bail!("`{DEFERRED_INIT_SECTION}` section is truncated");
                }
                let n = u32::from_le_bytes(d[0..4].try_into().unwrap()) as usize;
                if d.len() < 4 + n * 4 {
                    bail!("`{DEFERRED_INIT_SECTION}` declares {n} units but is {} bytes", d.len());
                }
                return Ok((0..n)
                    .map(|i| u32::from_le_bytes(d[4 + i * 4..8 + i * 4].try_into().unwrap()))
                    .collect());
            }
        }
    }
    Ok(Vec::new())
}

/// Names of exported mutable globals, ordered by module global index.
///
/// Only *exported* globals are reachable through wasmtime's public API, so this
/// is also the documented requirement on the guest SDK: a mutable global that
/// is not exported is invisible to capture, and therefore invisible to
/// hibernation. Invariant 1 says an instance's mutable universe is "linear
/// memory, globals and tables"; this is where the engine can only honour the
/// part the guest agrees to expose.
pub(crate) fn exported_mutable_globals(wasm: &[u8]) -> Result<Vec<String>> {
    let mut mutability: Vec<bool> = Vec::new();
    let mut imported_globals = 0usize;
    let mut exports: Vec<(String, u32)> = Vec::new();

    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            wasmparser::Payload::ImportSection(r) => {
                // wasmparser 0.244 yields compact import *groups*; flatten.
                for imp in r.into_imports() {
                    if let wasmparser::TypeRef::Global(g) = imp?.ty {
                        imported_globals += 1;
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
    let _ = imported_globals;

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

/// Reject anything §4.3 V2/V3 forbids that the engine config cannot catch on
/// its own. The engine is not the validator — `wisp-control` owns V1-V4 — but a
/// second linear memory or an import outside the table must never get as far as
/// a slot, so the cheap structural checks are repeated here.
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
                    // V1: an undeclared import is an attempted capability. The
                    // linker would fail instantiation anyway; failing here names
                    // the offender instead of producing wasmtime's generic
                    // "unknown import".
                    //
                    // "Declared" means one of two enumerated tables, never a
                    // pattern: `wisp_abi::imports::IMPORTS`, or — since v1.4's
                    // §4.3 — `wisp_abi::pal::PAL_IMPORTS`, the WASI whitelist
                    // the deploy-time adapter composes (Appendix C, F7). A
                    // `wasi:*` name outside that whitelist is still an
                    // attempted capability and is still refused here; there is
                    // no `wasi:`-prefix escape hatch, which is the whole point
                    // of the table being a list rather than a rule.
                    let declared = wisp_abi::imports::is_declared(imp.module, imp.name)
                        || wisp_abi::pal::lookup(imp.module, imp.name).is_some();
                    if !declared {
                        bail!(
                            "V1: undeclared import `{}::{}` — imports are the complete \
                             capability surface (§4.3), and neither `wisp:platform` nor the \
                             WASI PAL whitelist declares this one",
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

/// Envelope for the content-addressed artifact cache (§4.4, §9.3).
///
/// The wasmtime `.cwasm` alone is not enough: a cell needs the ABI rev the
/// delta format is bound to, the deferred-unit list, and the global ordering,
/// and it must be able to check that the artifact was built by a compatible
/// engine before it maps code (§8.7).
#[derive(Serialize, Deserialize)]
pub(crate) struct ArtifactMeta {
    pub magic: [u8; 8],
    pub format: u32,
    pub abi_rev: u32,
    pub module_hash: [u8; 32],
    pub engine_fingerprint: String,
    pub memory_pages: u32,
    pub deferred: Vec<u32>,
    pub mut_global_names: Vec<String>,
    pub table_names: Vec<String>,
}

pub(crate) const ARTIFACT_MAGIC: [u8; 8] = *b"WISPTMPL";
pub(crate) const ARTIFACT_FORMAT: u32 = 1;

pub(crate) fn encode_artifact(meta: &ArtifactMeta, cwasm: &[u8]) -> Result<Vec<u8>> {
    let m = postcard::to_allocvec(meta).context("encoding artifact metadata")?;
    let mut out = Vec::with_capacity(m.len() + cwasm.len() + 12);
    out.extend_from_slice(&(m.len() as u32).to_le_bytes());
    out.extend_from_slice(&m);
    out.extend_from_slice(cwasm);
    Ok(out)
}

pub(crate) fn decode_artifact(bytes: &[u8]) -> Result<(ArtifactMeta, &[u8])> {
    if bytes.len() < 4 {
        bail!("template artifact is truncated");
    }
    let n = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let end = 4usize.checked_add(n).context("artifact metadata length overflow")?;
    if bytes.len() < end {
        bail!("template artifact metadata is truncated");
    }
    let meta: ArtifactMeta =
        postcard::from_bytes(&bytes[4..end]).context("decoding artifact metadata")?;
    if meta.magic != ARTIFACT_MAGIC {
        bail!("not a WISP template artifact");
    }
    if meta.format != ARTIFACT_FORMAT {
        bail!("template artifact format {} is not {ARTIFACT_FORMAT}", meta.format);
    }
    Ok((meta, &bytes[end..]))
}
