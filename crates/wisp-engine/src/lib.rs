//! # `wisp-engine` — the sandbox boundary
//!
//! Everything that touches `wasmtime` lives here, so that the rest of the
//! platform can talk about *instances* without talking about engines. It owns
//! the three mechanisms the platform's economics rest on:
//!
//! * **Templates** — a pre-initialized memory image, page-aligned and mapped
//!   copy-on-write. Cold activation therefore skips assembly load, JIT, static
//!   init and config parse, because all four already happened at deploy time.
//! * **Slots** — activation is `mmap MAP_PRIVATE` + seal globals + bind the
//!   import vtable; reset is `madvise(MADV_DONTNEED)`. Marginal cost per
//!   instance is dirty pages, which is the whole density argument.
//! * **Budgets** — a dispatch is a synchronous call on a pool thread, metered
//!   by `CLOCK_THREAD_CPUTIME_ID` and killed by the epoch counter. Kills are
//!   abrupt by construction: there is no unwinding.
//!
//! ## How the mechanisms map onto wasmtime
//!
//! The `mmap`/`madvise` sentences above are not something this crate does by
//! hand. They are what wasmtime's **pooling allocator with `memory_init_cow`**
//! does, adopted wholesale. Concretely:
//! `WispEngine::activate` is a `Store` plus a `Linker::instantiate`, which maps
//! the module's memory image `MAP_PRIVATE` into a pre-reserved slot;
//! `InstanceOps::reset` drops the store, which returns the slot to the pool and
//! decommits it, and instantiates again. No code in this crate copies a
//! template image into an instance.
//!
//! ## API stability
//!
//! The types below are the seam `wisp-host` codes against. Signatures are
//! fixed; implementations are not.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use anyhow::{bail, Context};
use wasmtime::{CallHook, Extern, Linker, Store, UpdateDeadline};

use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::imports::exports as guest_exports;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;

pub mod bindings;
pub mod budget;
pub mod image;
pub mod instance;
pub mod pal;
pub mod snapshot;
pub mod template;

mod layout;
mod linker;
mod meter;
mod state;
mod watchdog;
mod wtconfig;

pub use bindings::{HostBindings, ImportCall};
pub use budget::{CpuBudget, DispatchOutcome, KillPhase, TrapInfo, TrapKind};
pub use image::{MemoryImage, PAGE};
pub use instance::Instance;
pub use meter::{
    clock_read_failures, thread_cpu_nanos, thread_cpu_resolution_nanos, try_thread_cpu_nanos,
};
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template, DEFERRED_INIT_SECTION};

use instance::Live;
use state::{BudgetExceeded, Kill, StoreState, WallCapExceeded};
use template::TemplateInner;

pub type Result<T> = anyhow::Result<T>;

/// Engine-wide configuration. The feature flags here are not tuning knobs —
/// they are rule **V2** of deploy-time validation enforced a second
/// time at the engine, so that a module which somehow slipped past the
/// validator still cannot use threads, shared memory or atomics.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Ceiling on concurrently resident instances in this cell.
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling, from the plan limits. One of the
    /// three factors in the structural residency bound.
    pub max_memory_bytes: u64,
    /// Bumped every `epoch_tick_us` by a watchdog thread. Exists to *deliver*
    /// deadline kills, not to deschedule-and-resume — the latter would need
    /// fibers, i.e. hidden native stacks, which invariant 3 forbids.
    pub epoch_tick_us: u64,
    /// The wasmtime instance-pooling design, adopted wholesale.
    pub pooling: bool,
    /// Cranelift bounds-check masking / pointer sanitization on loads.
    pub spectre_mitigations: bool,
    /// Reserved guard region enabling elision of explicit bounds checks.
    ///
    /// Dead `PROT_NONE` space placed *after* each slot's linear memory, never
    /// mapped and never usable by a guest. What it buys is codegen: Cranelift
    /// can drop the explicit bounds check on any access whose **static** offset
    /// lands inside it, because an out-of-bounds access faults on unmapped
    /// pages instead. It does not enlarge what a guest may address — that is
    /// `max_memory_bytes`, enforced by the pool and again by the store limiter
    /// on every `memory.grow` — and it cannot be grown at runtime, since
    /// `memory_reservation` is fixed at `max_memory_bytes` with
    /// `memory_may_move(false)`.
    ///
    /// ## 32 MiB, not 2 GiB — this was a density ceiling nobody had evaluated
    ///
    /// The pooling allocator reserves `max_instances × (max_memory_bytes +
    /// guard_size_bytes)` of address space up front, so this and
    /// `max_instances` are not independent. x86-64 gives ~128 TiB of user
    /// address space, and at a 2 GiB guard that ran out an order of magnitude
    /// below the densities this platform is built for. Measured, by asking
    /// `WispEngine::new` at each setting:
    ///
    /// | `max_instances` | guard 2 GiB | guard 32 MiB |
    /// |---:|---|---|
    /// | 10,000 | 20.8 TiB — ok | 1.5 TiB — ok |
    /// | 50,000 | 103.8 TiB — **refused** | 7.6 TiB — ok |
    /// | 100,000 | 207.5 TiB — **refused** | 15.3 TiB — ok |
    /// | 200,000 | 415.0 TiB — **refused** | 30.5 TiB — ok |
    ///
    /// A cell could not be *configured* past ~10,000 instances, whatever its
    /// RAM. 32 MiB is also wasmtime's own default, so the 2 GiB was this
    /// engine's deliberate raise; what it bought over 32 MiB is elision for
    /// static offsets between the two, and a wasm32 static offset is normally a
    /// struct-field displacement. **The throughput cost of the reduction has not
    /// been measured** — if a bounds-check regression ever shows up on the
    /// `echo`/`echo-cs` ladders, this is the first constant to suspect.
    ///
    /// Note this participates in [`WispEngine::artifact_key`] via the engine
    /// fingerprint: changing it invalidates every cached AOT artifact, which is
    /// correct — it changes generated code — but means a fleet-wide recompile.
    pub guard_size_bytes: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_instances: 10_000,
            max_memory_bytes: 128 << 20,
            epoch_tick_us: 200,
            pooling: true,
            spectre_mitigations: true,
            guard_size_bytes: 32 << 20,
        }
    }
}

/// A configured wasmtime engine plus its epoch watchdog and AOT code cache.
///
/// Code pages map read-only and are shared by every instance of a module on
/// the machine, so per-instance code cost is zero.
pub struct WispEngine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub(crate) wasmtime: wasmtime::Engine,
    pub(crate) cfg: EngineConfig,
    /// Built once. A `Linker<T>` is reusable across every store of the same
    /// `T`, and the per-instance context it binds lives in the store data
    /// rather than in the linker.
    pub(crate) linker: Linker<StoreState>,
    pub(crate) watchdog: watchdog::Watchdog,
    pub(crate) fingerprint: String,
}

impl WispEngine {
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        let wasmtime = wtconfig::build_engine(&cfg)?;
        let linker = linker::build_linker(&wasmtime).map_err(anyhow::Error::from)?;
        let watchdog = watchdog::Watchdog::start(wasmtime.clone(), cfg.epoch_tick_us);
        let fingerprint = wtconfig::engine_fingerprint(&cfg);
        Ok(Self {
            inner: Arc::new(EngineInner {
                wasmtime,
                cfg,
                linker,
                watchdog,
                fingerprint,
            }),
        })
    }

    /// Cranelift compile. Treated as parsing hostile input — the
    /// caller is expected to have run this inside the resource-capped
    /// validator sandbox.
    pub fn load_template(&self, template_wasm: &[u8]) -> Result<Template> {
        template::structural_check(template_wasm)?;
        let module = wasmtime::Module::new(&self.inner.wasmtime, template_wasm)
            .map_err(anyhow::Error::from)
            .context("compiling template module")?;
        let deferred = template::read_deferred_units(template_wasm)?;
        let mut_names = template::exported_mutable_globals(template_wasm)?;
        let table_names = template::exported_tables(template_wasm)?;
        self.finish_template(
            module,
            ModuleHash::of(template_wasm),
            deferred,
            mut_names,
            table_names,
        )
    }

    /// Serializes this template's metadata and compiled WebAssembly module.
    pub fn serialize(&self, t: &Template) -> Result<Vec<u8>> {
        let cwasm = t
            .inner
            .module
            .serialize()
            .map_err(anyhow::Error::from)
            .context("serializing compiled template")?;
        template::encode_artifact(&self.artifact_meta(t), &cwasm)
    }

    fn artifact_meta(&self, t: &Template) -> template::ArtifactMeta {
        template::ArtifactMeta {
            magic: template::ARTIFACT_MAGIC,
            format: template::ARTIFACT_FORMAT,
            abi_rev: t.abi_rev,
            module_hash: t.hash.0,
            engine_fingerprint: self.inner.fingerprint.clone(),
            memory_pages: t.inner.memory_pages,
            deferred: t.inner.deferred.clone(),
            mut_global_names: t.inner.mut_globals.iter().map(|(n, _)| n.clone()).collect(),
            table_names: t.inner.tables.iter().map(|(n, _)| n.clone()).collect(),
        }
    }

    /// The same artifact, as the two files a cell should actually hold: the
    /// metadata sidecar, and the bare `.cwasm`.
    ///
    /// ## Why this exists, and why the single-blob form cannot be mapped
    ///
    /// [`WispEngine::serialize`] prefixes the compiled module with a metadata
    /// header, so the blob's byte 0 is not the `.cwasm`'s byte 0. That is fine
    /// for [`WispEngine::deserialize`], which takes a slice — and fatal for
    /// [`WispEngine::deserialize_file`], which hands wasmtime a *path* so the
    /// code can be mapped from the page cache instead of copied into anonymous
    /// memory. `Module::deserialize_file` requires the artifact to start at
    /// offset 0 of the file it maps.
    ///
    /// Splitting them is worth a two-file artifact. On the C# bench guest's
    /// pre-initialized template — most of which is memory image that is never
    /// read — `deserialize(&[u8])` makes the whole artifact resident and mints
    /// a memfd the size of the image, while `deserialize_file(path)` makes
    /// almost none of it resident and mints no memfd at all. The memfd goes to
    /// zero because wasmtime sources the CoW image from the file mapping
    /// rather than minting one, and per-instance sharing is unchanged by that:
    /// the same marginal PSS per instance on both paths.
    pub fn serialize_parts(&self, t: &Template) -> Result<(Vec<u8>, Vec<u8>)> {
        let cwasm = t
            .inner
            .module
            .serialize()
            .map_err(anyhow::Error::from)
            .context("serializing compiled template")?;
        let meta =
            postcard::to_allocvec(&self.artifact_meta(t)).context("encoding artifact metadata")?;
        Ok((meta, cwasm))
    }

    /// Compile a template's wasm straight to the two artifact parts, without
    /// building a [`Template`] and without keeping the compiled module.
    ///
    /// This is the call a deploy step wants: it produces exactly what
    /// [`WispEngine::deserialize_file`] consumes, under *this* engine's
    /// configuration, so the artifact is loadable by the cells this engine
    /// describes. Compiling with a differently configured `wasmtime::Engine`
    /// produces a `.cwasm` that `deserialize` refuses — which is the check
    /// working, not a bug, and is why the compile belongs here rather than
    /// beside a second `Config`.
    pub fn precompile(&self, template_wasm: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        template::structural_check(template_wasm)?;
        let t = self.load_template(template_wasm)?;
        self.serialize_parts(&t)
    }

    /// # Safety
    /// The bytes must be unmodified output of [`WispEngine::serialize`] from
    /// the same engine version and configuration. The caller must establish
    /// this compiler origin before passing them to Wasmtime.
    pub unsafe fn deserialize(&self, bytes: &[u8]) -> Result<Template> {
        let (meta, cwasm) = template::decode_artifact(bytes)?;
        self.check_artifact_meta(&meta)?;
        // SAFETY: delegated to the caller's contract above.
        let module = unsafe { wasmtime::Module::deserialize(&self.inner.wasmtime, cwasm) }
            .map_err(anyhow::Error::from)
            .context("deserializing precompiled template")?;
        self.finish_template(
            module,
            ModuleHash(meta.module_hash),
            meta.deferred,
            meta.mut_global_names,
            meta.table_names,
        )
    }

    /// Load a template by **mapping** its compiled artifact from a file.
    ///
    /// The counterpart to [`WispEngine::serialize_parts`], and the load path a
    /// cell should use. `deserialize` copies the whole artifact into anonymous
    /// memory and makes every byte of it resident whether or not anything ever
    /// executes it; this maps the file, so the code and the memory image are
    /// page cache — demand-paged, evictable under pressure, and shared by every
    /// mapping of the same artifact rather than duplicated per tenant.
    ///
    /// The reason this is not merely tidier is on
    /// [`WispEngine::serialize_parts`].
    ///
    /// # Safety
    /// `meta` and the file at `cwasm_path` must be unmodified output of
    /// [`WispEngine::serialize_parts`] from the same engine version and
    /// configuration.
    ///
    /// The file is *mapped*, not read, so it must also not be modified while
    /// this template is alive — a truncation under the mapping is a SIGBUS in
    /// the middle of guest code, not an error return. A content-addressed
    /// store never rewrites a blob in place, which is what makes this safe
    /// where it is used; a caller pointing this at a mutable path is the one
    /// introducing the hazard.
    pub unsafe fn deserialize_file(
        &self,
        meta: &[u8],
        cwasm_path: &std::path::Path,
    ) -> Result<Template> {
        let meta: template::ArtifactMeta =
            postcard::from_bytes(meta).context("decoding artifact metadata")?;
        self.check_artifact_meta(&meta)?;
        // SAFETY: delegated to the caller's contract above.
        let module =
            unsafe { wasmtime::Module::deserialize_file(&self.inner.wasmtime, cwasm_path) }
                .map_err(anyhow::Error::from)
                .with_context(|| {
                    format!("mapping precompiled template {}", cwasm_path.display())
                })?;
        self.finish_template(
            module,
            ModuleHash(meta.module_hash),
            meta.deferred,
            meta.mut_global_names,
            meta.table_names,
        )
    }

    /// The two checks that stand between a cell and mapping code it cannot
    /// run: the engine that built the artifact, and the ABI rev the delta
    /// format is bound to.
    fn check_artifact_meta(&self, meta: &template::ArtifactMeta) -> Result<()> {
        if meta.magic != template::ARTIFACT_MAGIC {
            bail!("not a WISP template artifact");
        }
        if meta.format != template::ARTIFACT_FORMAT {
            bail!(
                "template artifact format {} is not {}",
                meta.format,
                template::ARTIFACT_FORMAT
            );
        }
        if meta.engine_fingerprint != self.inner.fingerprint {
            bail!(
                "template artifact was built by engine {} but this cell is {} — artifacts are \
                 content-addressed by (module hash, engine version, µarch)",
                meta.engine_fingerprint,
                self.inner.fingerprint
            );
        }
        if meta.abi_rev != wisp_abi::ABI_REV {
            bail!(
                "template artifact is ABI rev {} but this engine is rev {}",
                meta.abi_rev,
                wisp_abi::ABI_REV
            );
        }
        Ok(())
    }

    /// The artifact-cache key for a template on this engine:
    /// `(module hash, engine version, µarch)`.
    pub fn artifact_key(&self, t: &Template) -> String {
        format!("{}-{}", t.hash.hex(), self.inner.fingerprint)
    }

    /// The same key from the template *wasm*, without compiling it.
    ///
    /// The point of the cache is to answer "do I already have this?" before
    /// paying for a compile, which a key that needs a `Template` cannot do.
    pub fn artifact_key_for(&self, template_wasm: &[u8]) -> String {
        format!(
            "{}-{}",
            ModuleHash::of(template_wasm).hex(),
            self.inner.fingerprint
        )
    }

    /// Compile a template's wasm to the two artifact files, and drop the
    /// compiled module before returning.
    ///
    /// Dropping it is the point. Cranelift does not give its working set back:
    /// compiling the C# bench guest's pre-initialized template leaves tens of
    /// megabytes of anonymous memory per module resident in the process
    /// **after the template is dropped**, and `malloc_trim` recovers none of
    /// it. A process that compiles N templates carries N times that for its
    /// whole life. So the
    /// intended caller is a *short-lived* one — `wispd --precompile`, or a
    /// deploy step — after which the cell maps the result and never runs the
    /// compiler at all.
    pub fn precompile_to_files(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<ArtifactPaths> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key_for(template_wasm));
        let (meta, cwasm) = self.precompile(template_wasm)?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating artifact cache {}", dir.display()))?;
        // Written to a temporary name and renamed, because the `.cwasm` is
        // *mapped* by whoever loads it: a reader that opens a half-written file
        // does not get an error, it gets a SIGBUS later. `rename` within a
        // directory is atomic, so a reader sees the whole file or no file.
        write_atomic(&paths.meta, &meta)?;
        write_atomic(&paths.cwasm, &cwasm)?;
        Ok(paths)
    }

    /// Load a template by mapping its artifact, compiling one first only if the
    /// cache does not have it.
    ///
    /// See [`WispEngine::serialize_parts`] for the shared file representation
    /// and [`WispEngine::precompile_to_files`] for cache population.
    ///
    /// # Safety
    /// The cache directory must contain only artifacts this engine produced,
    /// and nothing may rewrite a file in it while a template is alive. Both
    /// hold for a directory the process owns and writes through
    /// [`WispEngine::precompile_to_files`], which is content-addressed and
    /// writes by atomic rename. A cache writable by an untrusted actor does
    /// not meet these compiler-origin and immutable-backing preconditions.
    pub unsafe fn load_mapped(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<(Template, bool)> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key_for(template_wasm));
        let mut compiled = false;
        if !(paths.meta.exists() && paths.cwasm.exists()) {
            self.precompile_to_files(template_wasm, dir)?;
            compiled = true;
        }
        let meta = std::fs::read(&paths.meta)
            .with_context(|| format!("reading {}", paths.meta.display()))?;
        // SAFETY: delegated to this function's contract.
        let t = unsafe { self.deserialize_file(&meta, &paths.cwasm) }?;
        Ok((t, compiled))
    }

    /// Map the template CoW, seal globals/tables, bind the import vtable to
    /// this instance's tenant context.
    ///
    /// The first two are performed by `Linker::instantiate` under
    /// `memory_init_cow` + the pooling allocator: the memory image is mapped
    /// `MAP_PRIVATE` into a pre-reserved slot and globals and tables are
    /// initialized from the module's sealed post-init values. The third is the
    /// `HostBindings` box landing in this store's data, which is where the
    /// meters and capability config for *this* tenant live.
    pub fn activate(&self, t: &Template, b: Box<dyn HostBindings>) -> Result<Instance> {
        let data = StoreState::new(b, self.inner.cfg.max_memory_bytes as usize);
        let live = instantiate(&self.inner, &t.inner, data)?;
        Ok(Instance {
            engine: self.inner.clone(),
            template: t.inner.clone(),
            live: Some(live),
            tombstone: None,
            last: None,
            last_violation: None,
        })
    }

    /// Drive the epoch counter by hand (tests and deterministic harnesses).
    pub fn increment_epoch(&self) {
        self.inner.wasmtime.increment_epoch();
    }

    /// Epoch bumps the watchdog has actually published. Reported because the
    /// achieved tick is not the configured one.
    pub fn epoch_ticks(&self) -> u64 {
        self.inner.watchdog.shared().ticks()
    }

    fn finish_template(
        &self,
        module: wasmtime::Module,
        hash: ModuleHash,
        deferred: Vec<u32>,
        mut_names: Vec<String>,
        table_names: Vec<String>,
    ) -> Result<Template> {
        // Instantiate once to read back the pre-initialized image. This is the
        // baseline the page-diff runs against and the oracle `reset` is checked
        // against; it is *not* what activation copies from, which is the CoW
        // mapping wasmtime already holds.
        let mut probe_state =
            StoreState::new(Box::new(DenyAll), self.inner.cfg.max_memory_bytes as usize);
        probe_state.clock_ms = 0;
        let mut store = Store::new(&self.inner.wasmtime, probe_state);
        store.limiter(|d| &mut d.limits);
        store.set_epoch_deadline(u64::MAX / 2);

        // Compose the WASI PAL adapter over whatever `wasi:*` the module
        // declares, before anything tries to instantiate it. A module that
        // declares none — every Rust-SDK guest — takes the engine's shared
        // linker unchanged and pays nothing.
        //
        // Composition happens here rather than at activation because a
        // `TrapOnCall` stub is typed from the module's own declaration, so it
        // needs the module; and because the result is a property of the
        // template, which every instance of the module on the machine shares.
        let needs_pal = module.imports().any(|i| pal::is_foreign_module(i.module()));
        let (pal_linker, pal_composed) = if needs_pal {
            let mut l = self.inner.linker.clone();
            let composed = pal::link(&mut l, &module).context(
                "composing the WASI PAL adapter; an import outside \
                 `wisp_abi::pal::PAL_IMPORTS` is a V1 rejection, not something the linker \
                 may improvise",
            )?;
            tracing::info!(
                composed = composed.len(),
                delegated = composed
                    .iter()
                    .filter(|d| d.disposition.is_delegated())
                    .count(),
                "composed the WASI PAL adapter for a template"
            );
            (Some(l), composed)
        } else {
            (None, Vec::new())
        };

        let inst = pal_linker
            .as_ref()
            .unwrap_or(&self.inner.linker)
            .instantiate(&mut store, &module)
            .map_err(anyhow::Error::from)
            .context("instantiating the template to read its pre-initialized image")?;

        let memory = inst
            .get_memory(&mut store, guest_exports::MEMORY)
            .with_context(|| format!("template does not export `{}`", guest_exports::MEMORY))?;
        // Scanned into runs rather than copied: see `image::MemoryImage`. The
        // dense borrow lives only for this statement, so a .NET template's
        // whole declared span is never *held* densely, only read once.
        let image = Arc::new(crate::image::MemoryImage::from_dense(memory.data(&store)));
        let memory_pages = memory.size(&store) as u32;

        let mut mut_globals = Vec::with_capacity(mut_names.len());
        for name in &mut_names {
            let g = inst
                .get_global(&mut store, name)
                .with_context(|| format!("exported mutable global `{name}` vanished"))?;
            let v = g.get(&mut store);
            mut_globals.push((name.clone(), instance_global_value(v, &store)?));
        }

        let mut tables = Vec::with_capacity(table_names.len());
        let mut table_nullness = Vec::with_capacity(table_names.len());
        for name in &table_names {
            let t = inst
                .get_table(&mut store, name)
                .with_context(|| format!("exported table `{name}` vanished"))?;
            let size = t.size(&store);
            let mut nulls = Vec::with_capacity(size as usize);
            for i in 0..size {
                nulls.push(match t.get(&mut store, i) {
                    Some(wasmtime::Ref::Func(f)) => f.is_some(),
                    Some(wasmtime::Ref::Extern(e)) => e.is_some(),
                    Some(_) => true,
                    None => false,
                });
            }
            tables.push((name.clone(), size));
            table_nullness.push(nulls);
        }

        let has_gc_hint = inst.get_func(&mut store, guest_exports::GC_HINT).is_some();
        let has_initialize = inst
            .get_func(&mut store, guest_exports::INITIALIZE)
            .is_some();
        drop(store);

        Ok(Template {
            hash,
            abi_rev: wisp_abi::ABI_REV,
            inner: Arc::new(TemplateInner {
                module,
                pal_linker,
                pal_composed,
                image,
                memory_pages,
                deferred,
                mut_globals,
                tables,
                table_nullness,
                has_gc_hint,
                has_initialize,
            }),
        })
    }
}

fn instance_global_value(
    v: wasmtime::Val,
    store: impl wasmtime::AsContext,
) -> Result<snapshot::GlobalValue> {
    use snapshot::GlobalValue as G;
    Ok(match v {
        wasmtime::Val::I32(x) => G::I32(x),
        wasmtime::Val::I64(x) => G::I64(x),
        wasmtime::Val::F32(x) => G::F32(x),
        wasmtime::Val::F64(x) => G::F64(x),
        other => bail!(
            "template mutable global of type {:?} is not capturable",
            other.ty(store)
        ),
    })
}

/// Where one template's two artifact files live.
///
/// Two files rather than one because `Module::deserialize_file` maps a path and
/// therefore needs the `.cwasm` to start at byte 0 — see
/// [`WispEngine::serialize_parts`].
#[derive(Debug, Clone)]
pub struct ArtifactPaths {
    pub meta: std::path::PathBuf,
    pub cwasm: std::path::PathBuf,
}

impl ArtifactPaths {
    pub fn new(dir: &std::path::Path, key: &str) -> Self {
        ArtifactPaths {
            meta: dir.join(format!("{key}.meta")),
            cwasm: dir.join(format!("{key}.cwasm")),
        }
    }
}

/// Write, then rename. A partially written `.cwasm` cannot be distinguished
/// from a whole one by a reader that maps it — the failure arrives later, as a
/// fault in the middle of guest code.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Bindings for the probe instantiation in `load_template`. Nothing may be
/// called: reading the template image must not have side effects, and a
/// template whose *instantiation* calls an import is a template that would bake
/// a host response into every clone.
struct DenyAll;

impl HostBindings for DenyAll {
    fn call_future(&mut self, _call: ImportCall<'_>) -> u64 {
        0
    }
    fn call_sync(&mut self, _call: ImportCall<'_>) -> i64 {
        0
    }
    fn slice_clock_ms(&self) -> u64 {
        0
    }
}

/// The shared activation path: a `Store`, the hooks, an instantiation, and the
/// export resolution the hot path needs.
pub(crate) fn instantiate(
    engine: &Arc<EngineInner>,
    t: &Arc<TemplateInner>,
    data: StoreState,
) -> Result<Live> {
    let mut store = Store::new(&engine.wasmtime, data);

    // The plan's linear-memory ceiling.
    store.limiter(|d| &mut d.limits);

    // Checked, not assumed: the depth behind `is_parked` is maintained
    // by wasmtime's own transition hook, so it reflects the engine's view of
    // the native stack rather than the engine embedder's bookkeeping around a
    // call. It costs a closure call per host/wasm transition, which is a real
    // price paid for a real property.
    store.call_hook(|mut ctx, hook| {
        let d = ctx.data_mut();
        match hook {
            CallHook::CallingWasm => {
                d.wasm_depth += 1;
                d.max_wasm_depth = d.max_wasm_depth.max(d.wasm_depth);
            }
            CallHook::ReturningFromWasm => d.wasm_depth -= 1,
            _ => {}
        }
        Ok(())
    });

    // The CPU-budget kill is thread-clock metered and epoch-delivered.
    // Epochs are wall-clock ticks and cannot themselves express a CPU budget;
    // what they give us is a periodic, guest-preempting callback in which the
    // *thread* clock is the thing compared. Under preemption a dispatch may
    // burn many epochs while off-CPU and is not killed for it, which is the
    // same reason wall time does not consume the CPU budget.
    //
    // The two-phase kill lives in these twenty lines, and the ORDER of the
    // two comparisons is the whole guarantee:
    //
    //   1. **B+G first, unconditionally.** The hard deadline ignores the
    //      runtime layer's *unsafe-to-trap* bit and ignores whether
    //      grace was ever successfully entered. A guest that pins the bit, or
    //      one whose host declines to offer grace at all, still dies here.
    //      That is what makes the grace window safe to offer.
    //   2. **B second, and it does not trap.** Crossing B enters grace: the
    //      host completes the cancellation future and poisons this dispatch's
    //      imports, and the guest is allowed to keep running so it can unwind
    //      and return. Entry *defers* while the unsafe-to-trap bit is set,
    //      because delivering `Cancelled` into a half-mutated GC or scheduler
    //      queue is precisely the corruption tombstones exist to contain.
    store.epoch_deadline_callback(|mut ctx| {
        let m = ctx.data().meter;
        let cpu = m.dispatch_cpu_ns(thread_cpu_nanos());

        // Phase 2 — the hard deadline. Bit-blind by construction.
        if cpu >= m.hard_ns() {
            ctx.data_mut().kill = Some(Kill::CpuHard);
            return Err(BudgetExceeded {
                cpu_ns: cpu,
                budget_ns: m.cpu_budget_ns,
                grace_ns: m.grace_ns,
            }
            .into());
        }

        // Phase 1 — grace entry. At most once per slice, and only when the
        // host was actually told: a deferral leaves `grace_at_ns` unset so the
        // next tick tries again, which is what bounds a pinned bit to G.
        if cpu >= m.cpu_budget_ns && ctx.data().grace_at_ns.is_none() {
            if ctx.data().unsafe_to_trap {
                ctx.data_mut().grace_deferrals += 1;
            } else if let Some(mut b) = ctx.data_mut().bindings.take() {
                // The vtable is moved out for the duration, exactly as an
                // import call does, so `enter_grace` cannot observe a
                // half-borrowed store. It is put back before returning to the
                // guest whatever happens.
                b.enter_grace();
                let d = ctx.data_mut();
                d.bindings = Some(b);
                d.grace_at_ns = Some(cpu);
            }
            // `bindings` is `None` only while an import call is in flight, and
            // epoch checks are emitted into *wasm* code, not into host
            // functions — so that branch is unreachable in practice. If it
            // ever is reached, leaving `grace_at_ns` unset simply retries on
            // the next tick, well inside G.
        }

        if let Some(w) = m.wall0 {
            let wall = w.elapsed().as_nanos() as u64;
            if wall >= m.wall_cap_ns {
                ctx.data_mut().kill = Some(Kill::Wall);
                return Err(WallCapExceeded { wall_ns: wall }.into());
            }
        }
        Ok(UpdateDeadline::Continue(m.check_ticks))
    });

    // Instantiation itself runs no guest code for a well-formed template, but
    // the deadline must be stood up regardless: with epoch interruption on, a
    // store starts with an already-expired deadline.
    store.set_epoch_deadline(u64::MAX / 2);

    let inst = t
        .pal_linker
        .as_ref()
        .unwrap_or(&engine.linker)
        .instantiate(&mut store, &t.module)
        .map_err(anyhow::Error::from)
        .context("activating template (an import outside the table fails here — V1)")?;

    // One pass over the export table; the hot path must not do string lookups.
    let mut memory = None;
    let mut f_alloc = None;
    let mut f_handle = None;
    let mut f_wake = None;
    let mut f_gc_hint = None;
    let mut f_initialize = None;
    {
        let exports: Vec<(String, Extern)> = inst
            .exports(&mut store)
            .map(|e| (e.name().to_string(), e.into_extern()))
            .collect();
        for (name, ext) in exports {
            match (name.as_str(), ext) {
                (guest_exports::MEMORY, Extern::Memory(m)) => memory = Some(m),
                (guest_exports::ALLOC, Extern::Func(f)) => f_alloc = Some(f),
                (guest_exports::HANDLE, Extern::Func(f)) => f_handle = Some(f),
                (guest_exports::WAKE, Extern::Func(f)) => f_wake = Some(f),
                (guest_exports::GC_HINT, Extern::Func(f)) => f_gc_hint = Some(f),
                (guest_exports::INITIALIZE, Extern::Func(f)) => f_initialize = Some(f),
                _ => {}
            }
        }
    }

    let memory = memory.with_context(|| {
        format!(
            "module does not export `{}` (required)",
            guest_exports::MEMORY
        )
    })?;
    let want = |o: Option<wasmtime::Func>, n: &str| -> Result<wasmtime::Func> {
        o.with_context(|| format!("module does not export `{n}` (required)"))
    };
    let f_alloc = want(f_alloc, guest_exports::ALLOC)?
        .typed::<(i32, i32), i32>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i32,i32)->i32", guest_exports::ALLOC))?;
    let f_handle = want(f_handle, guest_exports::HANDLE)?
        .typed::<(i32, i32), i64>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i32,i32)->i64", guest_exports::HANDLE))?;
    let f_wake = want(f_wake, guest_exports::WAKE)?
        .typed::<(i64, i32, i32), i64>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i64,i32,i32)->i64", guest_exports::WAKE))?;
    let f_gc_hint = match f_gc_hint {
        Some(f) => Some(
            f.typed::<i32, ()>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("`{}` must be (i32)->()", guest_exports::GC_HINT))?,
        ),
        None => None,
    };
    let f_initialize = match f_initialize {
        Some(f) => Some(
            f.typed::<(), ()>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("`{}` must be ()->()", guest_exports::INITIALIZE))?,
        ),
        None => None,
    };

    let mut globals = Vec::with_capacity(t.mut_globals.len());
    for (name, _) in &t.mut_globals {
        globals.push(
            inst.get_global(&mut store, name)
                .with_context(|| format!("exported mutable global `{name}` vanished"))?,
        );
    }
    let mut tables = Vec::with_capacity(t.tables.len());
    for (name, _) in &t.tables {
        tables.push(
            inst.get_table(&mut store, name)
                .with_context(|| format!("exported table `{name}` vanished"))?,
        );
    }

    store.data_mut().memory = Some(memory);

    Ok(Live {
        store,
        memory,
        f_alloc,
        f_handle,
        f_wake,
        f_gc_hint,
        f_initialize,
        globals,
        tables,
    })
}

/// What the host must be able to do to a live instance. Kept as a trait so
/// `wisp-host` can be written and tested against a fake engine.
pub trait InstanceOps {
    /// Run a dispatch to completion, park, or kill.
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Deliver a completed host future. Order between distinct futures is
    /// unspecified by contract.
    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Called before hibernation capture so the guest GC can compact
    /// into low addresses and zero the freed tail — zeroed pages compress out
    /// of the delta.
    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome;
    /// Reset: `madvise(MADV_DONTNEED)` restores template contents in
    /// microseconds and globals/tables are re-sealed.
    fn reset(&mut self) -> Result<()>;
    /// True only when this instance came to rest by returning `pending` and
    /// its WASM stack is empty — checked, not assumed.
    ///
    /// This is invariant 3's predicate — *suspension implies an empty stack* —
    /// and it is deliberately about **suspension**, not about eligibility.
    fn is_parked(&self) -> bool;

    /// True when this instance is at a serializable point and may be captured:
    /// the shadow stack pointer is at its base — an empty stack — and nothing
    /// about which return value produced it.
    ///
    /// Distinct from [`InstanceOps::is_parked`], and the distinction cost a
    /// real bug. Capture is eligible only on an empty stack, checked rather
    /// than assumed, and that is the whole of the definition: what a capture
    /// depends on is the stack being empty, not how the instance got there.
    /// An instance that returned `done` also has an empty
    /// stack — in fact it is the *easier* case, because there are no pending
    /// futures for the capture sequence to quiesce first.
    ///
    /// Requiring `pending` excluded exactly the instances hibernation is
    /// about: a Durable Instance that answers a message and goes idle would
    /// never have been capturable, so "a DI parked beyond `hibernate_after` is
    /// captured with no tenant involvement" would have been true only of DIs
    /// that happened to be waiting on something.
    ///
    /// `Trapped` and `BudgetKill` are excluded, and they are the whole reason
    /// this is not simply "the stack is empty": a hard kill unwinds nothing, so
    /// the memory of a killed instance is arbitrary — storing it would persist
    /// a state no execution ever produced. That is exactly the line
    /// `Completion::instance_survives` already draws.
    ///
    /// Implementations MUST still check the stack rather than infer it from
    /// the completion. Checked, not assumed.
    fn is_capture_eligible(&self) -> bool;
    fn memory(&mut self) -> &mut [u8];
    fn memory_len(&self) -> usize;
    /// Capture globals + tables. Linear memory is diffed separately by
    /// `wisp-hibernate` so the delta format stays that crate's business.
    fn capture_mutable_state(&mut self) -> Result<InstanceImage>;
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()>;
}

/// Classification of a completed dispatch, for the tenant-facing outcome
/// codes and the hang breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Guest returned `done`.
    Done,
    /// Guest returned `pending` — PARKED, empty stack, futures registered.
    Parked,
    /// Guest faulted. Instance discarded; never retried on the same memory.
    Trapped,
    /// The **hard** kill: still executing at B+G, terminated with no
    /// unwinding whatsoever. The instance is **tombstoned** — its memory is a
    /// state no execution ever produced.
    BudgetKill,
    /// The **clean** kill: the dispatch entered grace and *returned* inside
    /// it. `finally` ran, the instance is consistent, the slot is reusable,
    /// and the overage CPU is recorded.
    ///
    /// The request still fails with `budget_exceeded` — a clean kill is a
    /// kill. What it is not is a tombstone, and it does not count toward the
    /// hang breaker: it is the budget working as designed.
    CleanKill,
}

impl Completion {
    pub fn from_status(s: PollStatus) -> Self {
        match s {
            PollStatus::Done { .. } => Completion::Done,
            PollStatus::Pending => Completion::Parked,
            PollStatus::Trapped { .. } => Completion::Trapped,
        }
    }
    /// Whether the instance may be reused. A hard-killed or trapped instance
    /// may not: its memory is arbitrary and no cleanup ran. A **clean**-killed
    /// one may — that is the entire point of the grace window.
    pub fn instance_survives(self) -> bool {
        matches!(
            self,
            Completion::Done | Completion::Parked | Completion::CleanKill
        )
    }
    /// Whether this completion must leave a **tombstone**: a hard-killed
    /// instance may be internally inconsistent because the trap landed at an
    /// arbitrary back-edge — mid-collection, mid-queue-mutation, mid-frame —
    /// and the engine does not poison a store after a trap, so the platform
    /// must.
    pub fn tombstones(self) -> bool {
        matches!(self, Completion::Trapped | Completion::BudgetKill)
    }
}
