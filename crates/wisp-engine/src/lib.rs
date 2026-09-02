//! The sandbox boundary: everything that touches wasmtime. Templates are
//! pre-initialised memory images mapped copy-on-write by wasmtime's pooling
//! allocator; an activation is a `Store` plus `Linker::instantiate` into a
//! pre-reserved slot, and a reset drops the store and instantiates again. A
//! dispatch is a synchronous call on the caller's thread, metered by the
//! thread CPU clock and killed through the epoch counter.

use std::sync::atomic::{AtomicBool, Ordering};
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
pub use meter::clock_read_failures;
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template, DEFERRED_INIT_SECTION};

use instance::Live;
use state::{BudgetExceeded, Kill, StoreState, WallCapExceeded};
use template::TemplateInner;

pub type Result<T> = anyhow::Result<T>;

/// Engine-wide configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Ceiling on concurrently resident instances in this cell.
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling.
    pub max_memory_bytes: u64,
    /// Period of the epoch watchdog. Grace is delivered by epoch ticks, so
    /// [`WispEngine::new`] refuses a tick too coarse to fit
    /// [`budget::MIN_GRACE_TICKS`] inside [`budget::GRACE_FLOOR_NS`].
    pub epoch_tick_us: u64,
    pub pooling: bool,
    /// Cranelift bounds-check masking and pointer sanitisation on loads.
    pub spectre_mitigations: bool,
    /// `PROT_NONE` space after each slot's linear memory, never mapped, so
    /// Cranelift can drop the bounds check on any access whose static offset
    /// lands inside it. It does not enlarge what a guest may address. The
    /// pooling allocator reserves `max_instances * (max_memory_bytes + this)`
    /// of address space up front, so at 2 GiB a cell could not be configured
    /// past about ten thousand instances on x86-64; 32 MiB is wasmtime's own
    /// default and leaves room for the densities this platform is built for.
    /// Changing it changes generated code and so invalidates every cached
    /// artifact through the engine fingerprint.
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

/// A configured wasmtime engine plus its epoch watchdog.
pub struct WispEngine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub(crate) wasmtime: wasmtime::Engine,
    pub(crate) cfg: EngineConfig,
    /// Built once; a `Linker<T>` is reusable across every store of the same
    /// `T`, and the per-instance context lives in the store data.
    pub(crate) linker: Linker<StoreState>,
    pub(crate) watchdog: watchdog::Watchdog,
    pub(crate) fingerprint: String,
}

impl WispEngine {
    /// Fails on a configuration wasmtime rejects, on an epoch tick too coarse
    /// to deliver the grace window, and on an engine that turns out to accept
    /// threads or shared memory.
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        budget::check_grace_deliverable(cfg.epoch_tick_us)?;
        let wasmtime = wtconfig::build_engine(&cfg)?;
        wtconfig::assert_feature_set(&wasmtime)?;
        let linker = linker::build_linker(&wasmtime).map_err(anyhow::Error::from)?;
        let watchdog = watchdog::Watchdog::start(wasmtime.clone(), cfg.epoch_tick_us)?;
        let fingerprint = wtconfig::engine_fingerprint(&wasmtime, &cfg);
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

    /// Cranelift compile. Parsing hostile input: the caller runs this inside
    /// the resource-capped validator sandbox.
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

    /// Compile a template's wasm to the two artifact parts, the metadata
    /// sidecar and the bare `.cwasm`, under this engine's configuration.
    /// Compiling with a differently configured engine produces a `.cwasm` the
    /// load path refuses. Two files because `Module::deserialize_file` maps a
    /// path and needs the `.cwasm` to start at byte 0.
    pub fn precompile(&self, template_wasm: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let t = self.load_template(template_wasm)?;
        let cwasm = t
            .inner
            .module
            .serialize()
            .map_err(anyhow::Error::from)
            .context("serializing compiled template")?;
        let meta = template::ArtifactMeta {
            magic: template::ARTIFACT_MAGIC,
            format: template::ARTIFACT_FORMAT,
            abi_rev: t.abi_rev,
            module_hash: t.hash.0,
            engine_fingerprint: self.inner.fingerprint.clone(),
            cwasm_hash: *blake3::hash(&cwasm).as_bytes(),
            memory_pages: t.inner.memory_pages,
            deferred: t.inner.deferred.clone(),
            mut_global_names: t.inner.mut_globals.iter().map(|(n, _)| n.clone()).collect(),
            table_names: t.inner.tables.iter().map(|(n, _)| n.clone()).collect(),
        };
        let meta = postcard::to_allocvec(&meta).context("encoding artifact metadata")?;
        Ok((meta, cwasm))
    }

    /// Load a template by mapping its compiled artifact, so the code and the
    /// memory image are page cache rather than a private copy per process.
    ///
    /// The `.cwasm` is checked against the hash in `meta` before anything is
    /// mapped, so a tampered or truncated file is refused with an error. What
    /// the check cannot cover is `meta` itself, hence the unsafety.
    ///
    /// # Safety
    /// `meta` must come from a source the caller trusts to have been written
    /// by [`WispEngine::precompile`]: a crafted metadata plus a matching
    /// `.cwasm` bypasses wasm validation and runs as native code. The file is
    /// mapped, not read, so nothing may rewrite it while the template is
    /// alive; a truncation under the mapping is a fault inside guest code, not
    /// an error return.
    pub unsafe fn deserialize_file(
        &self,
        meta: &[u8],
        cwasm_path: &std::path::Path,
    ) -> Result<Template> {
        let meta: template::ArtifactMeta =
            postcard::from_bytes(meta).context("decoding artifact metadata")?;
        self.check_artifact_meta(&meta)?;
        let cwasm = std::fs::read(cwasm_path)
            .with_context(|| format!("reading {}", cwasm_path.display()))?;
        if *blake3::hash(&cwasm).as_bytes() != meta.cwasm_hash {
            bail!(
                "{} does not match the hash in its metadata; the artifact was tampered with or \
                 truncated and will not be mapped",
                cwasm_path.display()
            );
        }
        drop(cwasm);
        // SAFETY: the file's contents were hashed against the metadata just
        // above, and the caller vouches for the metadata (see the doc comment).
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
                "template artifact was built by engine {} but this cell is {}; artifacts are \
                 keyed by module hash and engine fingerprint",
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

    /// The artifact-cache key for a template on this engine, from the wasm
    /// alone so the cache can be consulted before paying for a compile.
    fn artifact_key(&self, template_wasm: &[u8]) -> String {
        format!(
            "{}-{}",
            ModuleHash::of(template_wasm).hex(),
            self.inner.fingerprint
        )
    }

    /// Compile a template's wasm to the two artifact files and drop the
    /// compiled module. Cranelift does not return its working set, so the
    /// intended caller is a short-lived process, after which a cell maps the
    /// result and never runs the compiler.
    pub fn precompile_to_files(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<ArtifactPaths> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key(template_wasm));
        let (meta, cwasm) = self.precompile(template_wasm)?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating artifact cache {}", dir.display()))?;
        write_atomic(&paths.meta, &meta)?;
        write_atomic(&paths.cwasm, &cwasm)?;
        Ok(paths)
    }

    /// Load a template by mapping its artifact, compiling one first only when
    /// the cache does not have it. Returns whether a compile happened.
    ///
    /// # Safety
    /// `dir` must be written only by this process through
    /// [`WispEngine::precompile_to_files`]. A cache another writer can reach
    /// can hand this cell arbitrary code; see [`WispEngine::deserialize_file`].
    pub unsafe fn load_mapped(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<(Template, bool)> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key(template_wasm));
        let mut compiled = false;
        if !(paths.meta.exists() && paths.cwasm.exists()) {
            self.precompile_to_files(template_wasm, dir)?;
            compiled = true;
        }
        let meta = std::fs::read(&paths.meta)
            .with_context(|| format!("reading {}", paths.meta.display()))?;
        // SAFETY: the caller vouches for `dir`, which is where `meta` came from.
        let t = unsafe { self.deserialize_file(&meta, &paths.cwasm) }?;
        Ok((t, compiled))
    }

    /// Map the template copy-on-write and bind `b` as this instance's import
    /// vtable.
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

    fn finish_template(
        &self,
        module: wasmtime::Module,
        hash: ModuleHash,
        deferred: Vec<u32>,
        mut_names: Vec<String>,
        table_names: Vec<String>,
    ) -> Result<Template> {
        // Instantiate once to read back the pre-initialised image: the
        // baseline the page diff runs against. Activation maps wasmtime's own
        // CoW image and never copies from this.
        let called_import = Arc::new(AtomicBool::new(false));
        let mut probe_state = StoreState::new(
            Box::new(DenyAll {
                called: called_import.clone(),
            }),
            self.inner.cfg.max_memory_bytes as usize,
        );
        probe_state.clock_ms = 0;
        let mut store = Store::new(&self.inner.wasmtime, probe_state);
        store.limiter(|d| &mut d.limits);
        store.set_epoch_deadline(u64::MAX / 2);

        // The PAL adapter is composed per template: a trap stub is typed from
        // the module's own declaration, and every instance of the module
        // shares the result. A module importing only `wisp` takes the shared
        // linker.
        let needs_pal = module.imports().any(|i| pal::is_foreign_module(i.module()));
        let (pal_linker, pal_composed) = if needs_pal {
            let mut l = self.inner.linker.clone();
            let composed = pal::link(&mut l, &module)
                .context("composing the WASI PAL adapter for a template")?;
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
        if called_import.load(Ordering::Relaxed) {
            bail!(
                "template instantiation called a host import; a template whose start function \
                 reaches the host would bake one host response into every instance"
            );
        }

        let memory = inst
            .get_memory(&mut store, guest_exports::MEMORY)
            .with_context(|| format!("template does not export `{}`", guest_exports::MEMORY))?;
        let image = Arc::new(crate::image::MemoryImage::from_dense(memory.data(&store)));
        let memory_pages = memory.size(&store) as u32;

        let mut mut_globals = Vec::with_capacity(mut_names.len());
        for name in &mut_names {
            let g = inst
                .get_global(&mut store, name)
                .with_context(|| format!("exported mutable global `{name}` vanished"))?;
            let v = g.get(&mut store);
            mut_globals.push((name.clone(), snapshot::GlobalValue::of(v, &store)?));
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

/// Where one template's two artifact files live.
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

/// Write to a temporary name and rename, so a reader that maps the file sees
/// the whole file or no file. A half-written `.cwasm` faults inside guest code
/// rather than failing to load.
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

/// Bindings for the probe instantiation in `finish_template`. Records that
/// an import was called so the template can be refused; reading the image
/// must have no side effects.
struct DenyAll {
    called: Arc<AtomicBool>,
}

impl HostBindings for DenyAll {
    fn call_future(&mut self, _call: ImportCall<'_>) -> u64 {
        self.called.store(true, Ordering::Relaxed);
        0
    }
    fn call_sync(&mut self, _call: ImportCall<'_>) -> i64 {
        self.called.store(true, Ordering::Relaxed);
        0
    }
    fn slice_clock_ms(&self) -> u64 {
        0
    }
}

/// The shared activation path: a `Store`, the hooks, an instantiation, and
/// the export resolution the hot path needs.
pub(crate) fn instantiate(
    engine: &Arc<EngineInner>,
    t: &Arc<TemplateInner>,
    data: StoreState,
) -> Result<Live> {
    let mut store = Store::new(&engine.wasmtime, data);
    store.limiter(|d| &mut d.limits);

    // The depth behind `is_parked` comes from wasmtime's own transition hook,
    // so it reflects the engine's view of the native stack rather than this
    // crate's bookkeeping around a call.
    store.call_hook(|mut ctx, hook| {
        let d = ctx.data_mut();
        match hook {
            CallHook::CallingWasm => d.wasm_depth += 1,
            CallHook::ReturningFromWasm => d.wasm_depth -= 1,
            _ => {}
        }
        Ok(())
    });

    // Epochs are wall-clock ticks and cannot express a CPU budget; they give
    // a periodic, guest-preempting callback in which the thread clock is
    // compared. The order of the two comparisons is the two-phase kill: the
    // hard deadline at B+G first and unconditionally, ignoring the
    // unsafe-to-trap bit, so a pinned bit buys exactly the grace window; then
    // grace entry at B, which does not trap and defers while the bit is set,
    // because delivering `Cancelled` into a half-mutated GC is the corruption
    // tombstones exist to contain.
    store.epoch_deadline_callback(|mut ctx| {
        let m = ctx.data().meter;
        let cpu = m.dispatch_cpu_ns(meter::thread_cpu_nanos());

        if cpu >= m.hard_ns() {
            ctx.data_mut().kill = Some(Kill::CpuHard);
            return Err(BudgetExceeded {
                cpu_ns: cpu,
                budget_ns: m.cpu_budget_ns,
                grace_ns: m.grace_ns,
            }
            .into());
        }

        // A deferral leaves `grace_at_ns` unset so the next tick tries again.
        if cpu >= m.cpu_budget_ns && ctx.data().grace_at_ns.is_none() {
            if ctx.data().unsafe_to_trap {
                ctx.data_mut().grace_deferrals += 1;
            } else if let Some(mut b) = ctx.data_mut().bindings.take() {
                b.enter_grace();
                let d = ctx.data_mut();
                d.bindings = Some(b);
                d.grace_at_ns = Some(cpu);
            }
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

    // With epoch interruption on, a store starts with an expired deadline.
    store.set_epoch_deadline(u64::MAX / 2);

    let inst = t
        .pal_linker
        .as_ref()
        .unwrap_or(&engine.linker)
        .instantiate(&mut store, &t.module)
        .map_err(anyhow::Error::from)
        .context("activating template")?;

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

/// What the host does to a live instance. A trait so `wisp-host` can be
/// tested against a fake engine.
pub trait InstanceOps {
    /// Run a dispatch to completion, park, or kill.
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Deliver a completed host future.
    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Called before hibernation capture so the guest GC can compact and zero
    /// the freed tail; zeroed pages compress out of the delta.
    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome;
    /// Drop the store, which returns the slot to the pool, and instantiate
    /// the template again.
    fn reset(&mut self) -> Result<()>;
    /// True only when the last call returned `pending` and wasmtime reports
    /// no wasm frame on the native stack.
    fn is_parked(&self) -> bool;
    /// True when the instance may be captured: the stack is empty, the last
    /// call ended in a state the instance survives, and it is not tombstoned.
    /// Weaker than [`InstanceOps::is_parked`] on purpose: an instance that
    /// returned `done` and went idle is the case hibernation is about.
    fn is_capture_eligible(&self) -> bool;
    /// Linear memory; empty for an instance with no live store.
    fn memory(&mut self) -> &mut [u8];
    fn memory_len(&self) -> usize;
    /// Globals and tables. Linear memory is diffed by `wisp-hibernate`.
    fn capture_mutable_state(&mut self) -> Result<InstanceImage>;
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()>;
}

/// How a call into the guest ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Done,
    /// Returned `pending`: empty stack, futures registered.
    Parked,
    /// Guest fault. The instance is discarded.
    Trapped,
    /// Still executing at B+G and terminated without unwinding. The instance
    /// is tombstoned: its memory is a state no execution produced.
    BudgetKill,
    /// Entered grace and returned inside it. The request still fails, but the
    /// instance is consistent and its slot reusable.
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
    /// Whether the instance may be reused.
    pub fn instance_survives(self) -> bool {
        matches!(
            self,
            Completion::Done | Completion::Parked | Completion::CleanKill
        )
    }
    /// Whether the instance is tombstoned. wasmtime does not poison a store
    /// after a trap, so the platform does.
    pub fn tombstones(self) -> bool {
        matches!(self, Completion::Trapped | Completion::BudgetKill)
    }
}
