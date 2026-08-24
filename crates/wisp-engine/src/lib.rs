//! # `wisp-engine` — the sandbox boundary
//!
//! Everything that touches `wasmtime` lives here, so that the rest of the
//! platform can talk about *instances* without talking about engines. It owns
//! the three mechanisms the spec's economics rest on:
//!
//! * **Templates** (§4.4) — a pre-initialized memory image, page-aligned and
//!   mapped copy-on-write. Cold activation therefore skips assembly load,
//!   JIT, static init and config parse, because all four already happened at
//!   deploy time.
//! * **Slots** (§6.2) — activation is `mmap MAP_PRIVATE` + seal globals +
//!   bind the import vtable; reset is `madvise(MADV_DONTNEED)`. Marginal cost
//!   per instance is dirty pages, which is the whole of G1.
//! * **Budgets** (§6.4) — a dispatch is a synchronous call on a pool thread,
//!   metered by `CLOCK_THREAD_CPUTIME_ID` and killed by the epoch counter.
//!   Kills are abrupt by construction (§2.5.8): there is no unwinding.
//!
//! ## How the mechanisms map onto wasmtime
//!
//! The `mmap`/`madvise` sentences above are not something this crate does by
//! hand. They are what wasmtime's **pooling allocator with `memory_init_cow`**
//! does, and §6.2 says that design is "adopted wholesale". Concretely:
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
pub mod instance;
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
pub use instance::Instance;
pub use meter::{thread_cpu_nanos, thread_cpu_resolution_nanos};
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template, DEFERRED_INIT_SECTION};

use instance::Live;
use state::{BudgetExceeded, Kill, StoreState, WallCapExceeded};
use template::TemplateInner;

pub type Result<T> = anyhow::Result<T>;

/// Engine-wide configuration. The feature flags here are not tuning knobs —
/// they are rule **V2** of deploy-time validation (§4.3) enforced a second
/// time at the engine, so that a module which somehow slipped past the
/// validator still cannot use threads, shared memory or atomics.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Ceiling on concurrently resident instances in this cell.
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling (§5.5, §11.1). One of the three
    /// factors in the structural residency bound (§11.2).
    pub max_memory_bytes: u64,
    /// §6.4: bumped every 200 µs by a watchdog thread. Exists to *deliver*
    /// deadline kills, not to deschedule-and-resume — the latter would need
    /// fibers, i.e. hidden native stacks, which invariant 3 forbids.
    pub epoch_tick_us: u64,
    /// §6.2: the wasmtime instance-pooling design, adopted wholesale.
    pub pooling: bool,
    /// §8.4: Cranelift bounds-check masking / pointer sanitization on loads.
    pub spectre_mitigations: bool,
    /// Reserved guard region enabling elision of explicit bounds checks
    /// (§8.2.1).
    ///
    /// The pooling allocator reserves `max_instances × (max_memory_bytes +
    /// guard_size_bytes)` of virtual address space, so this and
    /// `max_instances` are not independent: at the 2 GiB default, 10,000 slots
    /// is ~20 TiB of `PROT_NONE` reservation. That costs no memory, but it is
    /// a real limit and `WispEngine::new` reports it if the kernel refuses.
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
            guard_size_bytes: 2 << 30,
        }
    }
}

/// A configured wasmtime engine plus its epoch watchdog and AOT code cache.
///
/// Code pages map read-only and are shared by every instance of a module on
/// the machine, so per-instance code cost is zero (§4.4).
pub struct WispEngine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub(crate) wasmtime: wasmtime::Engine,
    pub(crate) cfg: EngineConfig,
    /// Built once. A `Linker<T>` is reusable across every store of the same
    /// `T`, and the per-instance context it binds (§6.2 step 3) lives in the
    /// store data rather than in the linker.
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
            inner: Arc::new(EngineInner { wasmtime, cfg, linker, watchdog, fingerprint }),
        })
    }

    /// Cranelift compile. Treated as parsing hostile input (§8.7) — the
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
        self.finish_template(module, ModuleHash::of(template_wasm), deferred, mut_names, table_names)
    }

    /// Serializes this template's metadata and compiled WebAssembly module.
    pub fn serialize(&self, t: &Template) -> Result<Vec<u8>> {
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
            memory_pages: t.inner.memory_pages,
            deferred: t.inner.deferred.clone(),
            mut_global_names: t.inner.mut_globals.iter().map(|(n, _)| n.clone()).collect(),
            table_names: t.inner.tables.iter().map(|(n, _)| n.clone()).collect(),
        };
        template::encode_artifact(&meta, &cwasm)
    }

    /// # Safety
    /// The bytes must be unmodified output of [`WispEngine::serialize`] from
    /// the same engine version and configuration. The caller must establish
    /// this compiler origin before passing them to Wasmtime.
    pub unsafe fn deserialize(&self, bytes: &[u8]) -> Result<Template> {
        let (meta, cwasm) = template::decode_artifact(bytes)?;
        if meta.engine_fingerprint != self.inner.fingerprint {
            bail!(
                "template artifact was built by engine {} but this cell is {} — artifacts are \
                 content-addressed by (module hash, engine version, µarch) (§4.4)",
                meta.engine_fingerprint,
                self.inner.fingerprint
            );
        }
        if meta.abi_rev != wisp_abi::ABI_REV {
            bail!(
                "template artifact is ABI rev {} but this engine is rev {} (§6.6, §9.4)",
                meta.abi_rev,
                wisp_abi::ABI_REV
            );
        }
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

    /// The artifact-cache key for a template on this engine (§4.4, §9.3):
    /// `(module hash, engine version, µarch)`.
    pub fn artifact_key(&self, t: &Template) -> String {
        format!("{}-{}", t.hash.hex(), self.inner.fingerprint)
    }

    /// §6.2 step 1-3: map the template CoW, seal globals/tables, bind the
    /// import vtable to this instance's tenant context.
    ///
    /// Steps 1 and 2 are performed by `Linker::instantiate` under
    /// `memory_init_cow` + the pooling allocator: the memory image is mapped
    /// `MAP_PRIVATE` into a pre-reserved slot and globals and tables are
    /// initialized from the module's sealed post-init values. Step 3 is the
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
    /// achieved tick is not the configured one — see `docs/spike-findings.md`.
    pub fn epoch_ticks(&self) -> u64 {
        self.inner.watchdog.shared().ticks()
    }

    /// Fixed, per-live-instance bookkeeping the engine commits, independent of
    /// any instance. §6.2 targets ≤ 64 KB.
    pub fn bookkeeping_bytes_per_instance(&self) -> usize {
        wtconfig::BOOKKEEPING_BYTES
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
        // baseline §6.6 diffs against and the oracle `reset` is checked
        // against; it is *not* what activation copies from, which is the CoW
        // mapping wasmtime already holds.
        let mut probe_state =
            StoreState::new(Box::new(DenyAll), self.inner.cfg.max_memory_bytes as usize);
        probe_state.clock_ms = 0;
        let mut store = Store::new(&self.inner.wasmtime, probe_state);
        store.limiter(|d| &mut d.limits);
        store.set_epoch_deadline(u64::MAX / 2);

        let inst = self
            .inner
            .linker
            .instantiate(&mut store, &module)
            .map_err(anyhow::Error::from)
            .context("instantiating the template to read its pre-initialized image")?;

        let memory = inst
            .get_memory(&mut store, guest_exports::MEMORY)
            .with_context(|| format!("template does not export `{}`", guest_exports::MEMORY))?;
        let image = memory.data(&store).to_vec();
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
        let has_initialize = inst.get_func(&mut store, guest_exports::INITIALIZE).is_some();
        drop(store);

        Ok(Template {
            hash,
            abi_rev: wisp_abi::ABI_REV,
            inner: Arc::new(TemplateInner {
                module,
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

fn instance_global_value(v: wasmtime::Val, store: impl wasmtime::AsContext) -> Result<snapshot::GlobalValue> {
    use snapshot::GlobalValue as G;
    Ok(match v {
        wasmtime::Val::I32(x) => G::I32(x),
        wasmtime::Val::I64(x) => G::I64(x),
        wasmtime::Val::F32(x) => G::F32(x),
        wasmtime::Val::F64(x) => G::F64(x),
        other => bail!("template mutable global of type {:?} is not capturable", other.ty(store)),
    })
}

/// Bindings for the probe instantiation in `load_template`. Nothing may be
/// called: reading the template image must not have side effects, and a
/// template whose *instantiation* calls an import is a template that would bake
/// a host response into every clone (§4.4).
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

    // §5.5/§11.1: the plan's linear-memory ceiling.
    store.limiter(|d| &mut d.limits);

    // §6.6 "checked, not assumed". The depth behind `is_parked` is maintained
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

    // §4.5: "the CPU-budget kill (thread-clock metered, epoch-delivered)".
    // Epochs are wall-clock ticks and cannot themselves express a CPU budget;
    // what they give us is a periodic, guest-preempting callback in which the
    // *thread* clock is the thing compared. Under preemption a dispatch may
    // burn many epochs while off-CPU and is not killed for it, which is the
    // same reason wall time does not consume the CPU budget.
    //
    // §6.4's two-phase kill lives in these twenty lines, and the ORDER of the
    // two comparisons is the whole guarantee:
    //
    //   1. **B+G first, unconditionally.** The hard deadline ignores the
    //      runtime layer's *unsafe-to-trap* bit (§4.6) and ignores whether
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
            // the next tick, 200 µs later, well inside G.
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

    let inst = engine
        .linker
        .instantiate(&mut store, &t.module)
        .map_err(anyhow::Error::from)
        .context("activating template (an import outside the table fails here — §4.3 V1)")?;

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
        format!("module does not export `{}` (§5.2 required)", guest_exports::MEMORY)
    })?;
    let want = |o: Option<wasmtime::Func>, n: &str| -> Result<wasmtime::Func> {
        o.with_context(|| format!("module does not export `{n}` (§5.2 required)"))
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

    Ok(Live { store, memory, f_alloc, f_handle, f_wake, f_gc_hint, f_initialize, globals, tables })
}

/// What the host must be able to do to a live instance. Kept as a trait so
/// `wisp-host` can be written and tested against a fake engine.
pub trait InstanceOps {
    /// §6.2 step 4. Runs to completion, park, or kill.
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome;
    /// Deliver a completed host future (§5.2). Order between distinct futures
    /// is unspecified by contract (§5.4).
    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome;
    /// §5.5. Called before hibernation capture so the guest GC can compact
    /// into low addresses and zero the freed tail — zeroed pages compress out
    /// of the delta.
    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome;
    /// §6.2 reset: `madvise(MADV_DONTNEED)` restores template contents in
    /// microseconds and globals/tables are re-sealed.
    fn reset(&mut self) -> Result<()>;
    /// True only when this instance came to rest by returning `pending` and
    /// its WASM stack is empty (§6.6: "checked, not assumed").
    ///
    /// This is invariant 3's predicate — *suspension implies an empty stack* —
    /// and it is deliberately about **suspension**, not about eligibility. It
    /// is what claim C4 measures.
    fn is_parked(&self) -> bool;

    /// True when this instance is at a serializable point and may be captured
    /// (§6.6, and normative since spec v1.4: snapshot eligibility is
    /// `SP == base`, the shadow stack pointer at its base — an empty stack —
    /// and nothing about which return value produced it. Appendix C F1 cites
    /// this prototype for the finding.)
    ///
    /// Distinct from [`InstanceOps::is_parked`], and the distinction cost a
    /// real bug. §6.6 says capture is *"eligible only when Parked (empty stack
    /// — checked, not assumed)"*, and the parenthetical is the definition:
    /// what a capture depends on is the stack being empty, not how the
    /// instance got there. An instance that returned `done` also has an empty
    /// stack — in fact it is the *easier* case, because there are no pending
    /// futures for the capture sequence to quiesce first.
    ///
    /// Requiring `pending` excluded exactly the instances G3 is about: a
    /// Durable Instance that answers a message and goes idle would never have
    /// been capturable, so "a DI parked beyond `hibernate_after` is captured
    /// with no tenant involvement" (§6.6) would have been true only of DIs
    /// that happened to be waiting on something.
    ///
    /// `Trapped` and `BudgetKill` are excluded, and they are the whole reason
    /// this is not simply "the stack is empty": §2.5.8 kills a dispatch with no
    /// unwinding, so the memory of a killed instance is arbitrary — storing it
    /// would persist a state no execution ever produced. That is exactly the
    /// line `Completion::instance_survives` already draws.
    ///
    /// Implementations MUST still check the stack rather than infer it from
    /// the completion; §6.6 is explicit that this is checked, not assumed.
    fn is_capture_eligible(&self) -> bool;
    fn memory(&mut self) -> &mut [u8];
    fn memory_len(&self) -> usize;
    /// Capture globals + tables. Linear memory is diffed separately by
    /// `wisp-hibernate` so the delta format stays that crate's business.
    fn capture_mutable_state(&mut self) -> Result<InstanceImage>;
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()>;
}

/// Classification of a completed dispatch, for the tenant-facing contract in
/// §13 and the hang breaker in §6.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Guest returned `done`.
    Done,
    /// Guest returned `pending` — PARKED, empty stack, futures registered.
    Parked,
    /// Guest faulted. Instance discarded; never retried on the same memory.
    Trapped,
    /// §6.4's **hard** kill: still executing at B+G, terminated with no
    /// unwinding whatsoever (§2.5.8). The instance is **tombstoned** — its
    /// memory is a state no execution ever produced.
    BudgetKill,
    /// §6.4's **clean** kill: the dispatch entered grace and *returned* inside
    /// it. `finally` ran, the instance is consistent, the slot is reusable,
    /// and the overage CPU is recorded.
    ///
    /// The request still fails with `budget_exceeded` (§13) — a clean kill is
    /// a kill. What it is not is a tombstone, and it does not count toward the
    /// hang breaker: it is the budget working as designed (§6.4).
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
    /// one may — that is the entire point of the grace window (§6.4).
    pub fn instance_survives(self) -> bool {
        matches!(self, Completion::Done | Completion::Parked | Completion::CleanKill)
    }
    /// Whether §6.4 requires this completion to leave a **tombstone**: a
    /// hard-killed instance may be internally inconsistent because the trap
    /// landed at an arbitrary back-edge — mid-collection, mid-queue-mutation,
    /// mid-frame — and the engine does not poison a store after a trap
    /// (Appendix C, F2), so the platform must.
    pub fn tombstones(self) -> bool {
        matches!(self, Completion::Trapped | Completion::BudgetKill)
    }
}
