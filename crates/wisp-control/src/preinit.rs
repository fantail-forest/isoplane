//! Initialization inputs and captured state.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::imports::{self, ImportDef, Ty};
use wisp_engine::ModuleHash;
use wizer::wasmtime as wt;
use wizer::Wizer;

/// Initialization declared-memory limit.
pub use crate::validator::INIT_MEMORY_LIMIT_BYTES;

/// Maximum number of initialization attempts.
pub const MAX_PASSES: u32 = 16;

/// How many threads wizer's embedded wasmtime may compile on.
///
/// Left alone, wizer compiles through rayon's *global* pool, which sizes itself
/// to the machine — 256 workers on the box this was measured on. That pool is
/// shared with the data plane (`wisp-engine` runs wasmtime 48 through the same
/// rayon), so `RAYON_NUM_THREADS` is the wrong lever: it would also serialize
/// the optimizing AOT tier, whose whole purpose is to compile in the
/// background. [`PreInitializer::run`] therefore scopes a pool of its own with
/// `ThreadPool::install`, which binds only wizer's work.
///
/// One worker, because the parallelism was not buying anything. Sweeping
/// 1/2/4/8/16 over five guests spanning 55 KB to 15 MB moved wall time by less
/// than the run-to-run noise, while process CPU rose monotonically with the
/// thread count; the global pool was the *slowest* setting of all, costing
/// 1.6-2.0x the wall time of a single worker and 120-290x the CPU on the small
/// guests. The extra threads were spinning, not compiling.
///
/// One worker has a second effect worth naming: with nothing to steal the
/// work, the thread that drives the replay loop does all of it, so
/// [`PreInitOutcome::init_cpu_ms`] measures the whole of pre-init rather than
/// a floor. See [`ThreadCpu`].
pub const PREINIT_COMPILE_THREADS: usize = 1;

/// The host treats 0 and `u32::MAX` as no active initialization unit.
pub const NO_UNIT: u32 = u32::MAX;

/// The zero spelling of the no-active-unit sentinel.
pub const NO_UNIT_ALT: u32 = 0;

#[inline]
fn is_no_unit(id: u32) -> bool {
    id == NO_UNIT || id == NO_UNIT_ALT
}

/// Controls additional runtime PAL access during initialization.
/// Entropy is available under both modes, including inside initialization units;
/// random values captured in guest memory remain captured in that image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitPal {
    /// Baseline initialization imports and application entropy.
    #[default]
    Sealed,
    /// Additionally permit [`RUNTIME_PAL`] outside initialization units.
    RuntimeBringUp,
}

/// Additional PAL imports governed by runtime mode and unit scope.
pub const RUNTIME_PAL: &[&str] = &["clock_now_coarse", "entropy_fill"];

impl InitPal {
    pub fn serves(self, name: &str) -> bool {
        self == InitPal::RuntimeBringUp && RUNTIME_PAL.contains(&name)
    }
}

pub struct PreInitializer {
    memory_limit_bytes: u64,
    max_passes: u32,
    pal: InitPal,
    init_func: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreInitRequest {
    pub module_hash: ModuleHash,
    /// Serialized configuration available to initialization.
    pub init_blob: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PreInitOutcome {
    /// The pre-initialized module: same code, data segments replaced by the
    /// post-init memory image. This is what the data plane loads.
    pub template_wasm: Vec<u8>,
    /// Unit IDs recorded for the guest to skip on subsequent attempts.
    pub deferred_units: Vec<u32>,
    /// Number of initialization attempts, including the successful pass.
    pub passes: u32,
    /// CPU consumed by the thread that ran the replay loop, reported for the
    /// deploy record. Not a budget: nothing refuses a template for being slow.
    ///
    /// CPU rather than wall clock because this number is a property of the
    /// tenant's module, and wall clock on a shared control plane is a property
    /// of whoever else was deploying at the time. It counts the whole of
    /// pre-init rather than a floor only because the compile is scoped to one
    /// worker — see [`ThreadCpu`] and [`PREINIT_COMPILE_THREADS`], and for why
    /// `0` can also mean the clock was unreadable.
    pub init_cpu_ms: u64,
    pub template_bytes: u64,
    /// Digest of explicit initialization inputs; fresh random output is excluded.
    pub reproducible_digest: [u8; 32],
    /// Recorded unit IDs and the capabilities that caused their deferral.
    pub deferred_reasons: Vec<(u32, &'static str)>,
    /// `log_emit` output captured during the successful pass.
    pub init_logs: Vec<String>,
    /// Number of capability refusals across all attempts.
    pub capability_refusals: u32,
    /// PAL call counts from the successful initialization pass.
    pub pal_calls: Vec<(&'static str, u32)>,
    /// Hash-seed reads on the successful initialization pass.
    pub seed_reads: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PreInitError {
    #[error("module does not export `{0}`; nothing to pre-initialize")]
    NoInitFunc(String),
    /// The trap happened with no init unit in scope, so there is nothing to
    /// defer. Deferring a *guess* would silently change program behaviour.
    #[error(
        "pre-init trapped outside any init unit (`{import}`); \
             wrap initialization that may touch host capabilities in an init unit: {cause}"
    )]
    Unattributable {
        import: &'static str,
        cause: anyhow::Error,
    },
    #[error(
        "init unit {unit} trapped on `{import}` again after being deferred — \
             the guest is ignoring `init_unit_deferred`"
    )]
    DeferralIgnored { unit: u32, import: &'static str },
    #[error("pre-init did not converge in {0} passes")]
    NoConvergence(u32),
    #[error(
        "declared init memory of {bytes} bytes exceeds the initialization limit of {limit} bytes"
    )]
    MemoryLimit { bytes: u64, limit: u64 },
    #[error("pre-init trapped: {0}")]
    Trapped(anyhow::Error),
}

impl PreInitializer {
    pub fn new() -> Self {
        Self {
            memory_limit_bytes: INIT_MEMORY_LIMIT_BYTES,
            max_passes: MAX_PASSES,
            pal: InitPal::Sealed,
            init_func: None,
        }
    }

    /// Set the initialization declared-memory ceiling.
    pub fn with_memory_limit(memory_limit_bytes: u64) -> Self {
        Self {
            memory_limit_bytes,
            ..Self::new()
        }
    }

    /// Select the initialization PAL policy.
    pub fn with_pal(mut self, pal: InitPal) -> Self {
        self.pal = pal;
        self
    }

    /// Select the guest entry point to initialize; defaults to `wisp_initialize`.
    pub fn with_init_func(mut self, name: impl Into<String>) -> Self {
        self.init_func = Some(name.into());
        self
    }

    fn init_func_name(&self) -> &str {
        self.init_func
            .as_deref()
            .unwrap_or(imports::exports::INITIALIZE)
    }

    /// Run initialization, recording denied capabilities for the guest's unit protocol.
    ///
    /// The guest-side protocol, in full:
    ///
    /// ```text
    ///   init_unit_begin(id)                  ; host records "current unit = id"
    ///   if init_unit_deferred(id) == 0 {     ; host answers 1 if it wants this one skipped
    ///       ...unit body...                  ; may trap on a sealed capability
    ///   }
    ///   init_unit_begin(-1)                  ; optional: leave unit scope (NO_UNIT)
    /// ```
    ///
    /// The host treats 0 and `u32::MAX` as no active initialization unit.
    /// A refusal when no unit is recorded fails initialization.
    pub fn run(&self, wasm: &[u8], req: &PreInitRequest) -> crate::Result<PreInitOutcome> {
        if !exports_func(wasm, self.init_func_name()) {
            return Err(PreInitError::NoInitFunc(self.init_func_name().to_string()).into());
        }
        let declared = declared_memory_bytes(wasm);
        if declared > self.memory_limit_bytes {
            return Err(PreInitError::MemoryLimit {
                bytes: declared,
                limit: self.memory_limit_bytes,
            }
            .into());
        }

        // Bound wizer's compiler, and nothing else's. `install` runs the
        // closure on a worker of *this* pool, and rayon's implicit parallelism
        // — which is how wasmtime spells its parallel compile — resolves
        // against the pool of the calling thread rather than the global one.
        // So the data plane's `wisp-engine` engine and the deploy-time
        // optimizing AOT tier keep the global pool they were sized for.
        match compile_pool() {
            Some(pool) => pool.install(|| self.replay(wasm, req)),
            None => self.replay(wasm, req),
        }
    }

    /// The rollback-by-replay loop, run inside the scoped compile pool.
    ///
    /// Separate from [`Self::run`] so the CPU clock starts on the thread that
    /// actually does the work: `install` hands the closure to a pool worker
    /// and parks the caller, so a `ThreadCpu` started outside would measure
    /// the parked thread and report zero for every deploy.
    fn replay(&self, wasm: &[u8], req: &PreInitRequest) -> crate::Result<PreInitOutcome> {
        let mut deferred: BTreeSet<u32> = BTreeSet::new();
        let mut reasons: Vec<(u32, &'static str)> = Vec::new();
        let mut refusals = 0u32;
        let cpu = ThreadCpu::start();

        for pass in 1..=self.max_passes {
            let state = Arc::new(Mutex::new(Sandbox {
                seed: rand::random(),
                config: req.init_blob.clone(),
                deferred: deferred.clone(),
                pal: self.pal,
                ..Default::default()
            }));

            let result = wizen(wasm, self.init_func_name(), state.clone());
            let s = state.lock();
            refusals += s.refusals;

            match result {
                Ok(mut template_wasm) => {
                    // Recorded deferrals are not derivable from the wasm alone, and
                    // `wisp-engine`'s `Template::deferred_init_units()` has to
                    // answer it — so the fact travels *in the artifact*, not
                    // in a side channel the artifact could get separated from.
                    append_deferred_init_section(&mut template_wasm, &deferred);
                    // Thread CPU is reported after Wizer returns; this does not interrupt initialization.
                    let cpu_ms = cpu.elapsed_ms();
                    tracing::info!(
                        module = %req.module_hash,
                        passes = pass,
                        deferred = deferred.len(),
                        bytes = template_wasm.len(),
                        cpu_ms,
                        "pre-init converged"
                    );
                    return Ok(PreInitOutcome {
                        reproducible_digest: reproducible_digest(wasm, req),
                        template_bytes: template_wasm.len() as u64,
                        template_wasm,
                        deferred_units: deferred.iter().copied().collect(),
                        passes: pass,
                        init_cpu_ms: cpu_ms,
                        deferred_reasons: reasons,
                        init_logs: s.logs.clone(),
                        capability_refusals: refusals,
                        pal_calls: s.pal_calls.iter().map(|(k, v)| (*k, *v)).collect(),
                        seed_reads: s.seed_reads,
                    });
                }
                Err(e) => match s.violation {
                    // The interposer caught it: we know which unit reached for
                    // which capability, so defer that unit and replay.
                    Some((Some(unit), import)) if unit != NO_UNIT => {
                        // Already deferred and still trapping: the guest ran a
                        // unit it was told to skip. Replaying again would
                        // defer nothing new and loop, so say what is actually
                        // wrong instead.
                        if !deferred.insert(unit) {
                            return Err(PreInitError::DeferralIgnored { unit, import }.into());
                        }
                        reasons.push((unit, import));
                        tracing::debug!(
                            pass,
                            unit,
                            import,
                            "impure init unit deferred; replaying pre-init from scratch"
                        );
                    }
                    Some((_, import)) => {
                        return Err(PreInitError::Unattributable { import, cause: e }.into())
                    }
                    // Not our stub: a guest `abort`, an `unreachable`, an OOM.
                    // Nothing to defer, so the deploy fails — correctly.
                    None => return Err(PreInitError::Trapped(e).into()),
                },
            }
        }
        Err(PreInitError::NoConvergence(self.max_passes).into())
    }
}

impl Default for PreInitializer {
    fn default() -> Self {
        Self::new()
    }
}

/// A rayon pool of [`PREINIT_COMPILE_THREADS`] workers, or `None` if one could
/// not be built.
///
/// Built per call rather than kept in a `OnceLock`: a one-thread pool costs
/// ~51 us to build (median of 50, release profile), which is under 2% of the
/// fastest pre-init measured and invisible against the rest, and the per-call
/// pool leaves no idle threads in a daemon that deploys rarely.
///
/// `None` is a degraded path, not a failure: a control plane that cannot spawn
/// a thread has worse problems than an unbounded compile, and refusing the
/// deploy over a missed optimization would be the wrong trade. It is logged
/// because a silent fall back to the 256-worker pool would look exactly like
/// this fix never landing.
fn compile_pool() -> Option<rayon::ThreadPool> {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(PREINIT_COMPILE_THREADS)
        .thread_name(|i| format!("wisp-preinit-{i}"))
        .build()
    {
        Ok(pool) => Some(pool),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not build the pre-init compile pool; falling back to rayon's global pool"
            );
            None
        }
    }
}

/// The name of the custom section carrying [`PreInitOutcome::deferred_units`]
/// into the template (`wisp-engine` reads it as `Template::deferred_init_units`).
/// Payload: little-endian `u32` count, then that many little-endian `u32` ids.
/// An absent section means nothing was deferred.
pub const DEFERRED_INIT_SECTION: &str = "wisp.deferred-init";

/// Append the `wisp.deferred-init` custom section. A trailing custom section
/// is valid wasm and invisible to the engine's own decoding, which is why the
/// deferral record can ride along inside the artifact instead of beside it —
/// an artifact and its metadata that can be separated eventually are.
fn append_deferred_init_section(wasm: &mut Vec<u8>, deferred: &BTreeSet<u32>) {
    if deferred.is_empty() {
        return; // absent section == nothing deferred
    }
    let name = DEFERRED_INIT_SECTION.as_bytes();
    let mut body = Vec::with_capacity(name.len() + 8 + deferred.len() * 4);
    leb128_u32(&mut body, name.len() as u32);
    body.extend_from_slice(name);
    body.extend_from_slice(&(deferred.len() as u32).to_le_bytes());
    for id in deferred {
        body.extend_from_slice(&id.to_le_bytes());
    }
    wasm.push(0); // section id 0 = custom
    leb128_u32(wasm, body.len() as u32);
    wasm.extend_from_slice(&body);
}

/// Read the section back. `wisp-engine` has its own copy of this three-line
/// decode; this one exists so the *writer* has a round-trip test.
pub fn read_deferred_init_section(wasm: &[u8]) -> Vec<u32> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm).flatten() {
        if let wasmparser::Payload::CustomSection(c) = payload {
            if c.name() == DEFERRED_INIT_SECTION {
                let d = c.data();
                if d.len() < 4 {
                    return Vec::new();
                }
                let n = u32::from_le_bytes(d[..4].try_into().unwrap()) as usize;
                return d[4..]
                    .chunks_exact(4)
                    .take(n)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
            }
        }
    }
    Vec::new()
}

fn leb128_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            return;
        }
    }
}

/// Digest of explicit initialization inputs; fresh random output is excluded.
pub fn reproducible_digest(wasm: &[u8], req: &PreInitRequest) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"wisp.template.preimage.v1");
    h.update(&(wisp_abi::ABI_REV as u64).to_le_bytes());
    h.update(&ModuleHash::of(wasm).0);
    h.update(&(req.init_blob.len() as u64).to_le_bytes());
    h.update(&req.init_blob);
    *h.finalize().as_bytes()
}

// ---------------------------------------------------------------------------
// the sealed init sandbox
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Sandbox {
    seed: [u8; 16],
    config: Vec<u8>,
    deferred: BTreeSet<u32>,
    /// Set by `init_unit_begin`; cleared when a unit is skipped. This is the
    /// whole of the attribution mechanism.
    current_unit: Option<u32>,
    /// The unit most recently *told to skip*, cleared at the next
    /// `init_unit_begin`. It exists to tell two failures apart that otherwise
    /// look identical from outside: a guest that ran a unit it was told to
    /// skip (a protocol bug — deferral would loop forever), and a trap in
    /// glue code between units (unattributable — deferring a guess would
    /// silently change program behaviour).
    last_deferred: Option<u32>,
    /// `(unit in scope, capability reached for)` — recorded at the moment of
    /// refusal, because by the time the trap surfaces the stack is gone.
    violation: Option<(Option<u32>, &'static str)>,
    refusals: u32,
    logs: Vec<String>,
    seed_reads: u32,
    pal: InitPal,
    /// PAL call counts for this initialization pass.
    pal_calls: BTreeMap<&'static str, u32>,
    /// Counts host entropy draws during initialization.
    entropy_draws: u64,
}

impl Sandbox {
    fn serves(&self, name: &str) -> bool {
        name == "entropy_fill" || (self.current_unit.is_none() && self.pal.serves(name))
    }
}

/// CPU burned by the calling thread — `CLOCK_THREAD_CPUTIME_ID` — over the
/// life of the value.
///
/// ## Why the thread clock, and not wall time
///
/// `init_cpu_ms` lands on the deploy record, where it reads as a property of
/// the tenant's module. Wall clock is not that: on a control plane running
/// several deploys at once it moves with the neighbours' load, so the same
/// module reports a different init cost depending on who else showed up.
///
/// ## Why the thread clock, and not the process clock
///
/// The process clock is the same disease as wall clock, in a worse form:
/// `CLOCK_PROCESS_CPUTIME_ID` sums every thread in the daemon, so it charges a
/// deploy for the data plane's compiles and for whoever else was deploying.
///
/// ## Why it is no longer a floor
///
/// It used to be. Wizer's embedded wasmtime compiles through rayon, and with
/// the global pool in charge the work scattered across 256 workers that this
/// clock could not see — measured on a 256-core box, a 10.9 ms pre-init read
/// 7.6 ms on the thread clock and 802 ms on the process clock. Since
/// [`PreInitializer::run`] scopes that compile to
/// [`PREINIT_COMPILE_THREADS`] workers and this value is constructed *inside*
/// that scope, one worker means nothing can steal the work: across five guests
/// spanning 55 KB to 15 MB the thread clock, the process clock and wall time
/// now agree to within a few percent. Raising `PREINIT_COMPILE_THREADS` would
/// make it a floor again.
struct ThreadCpu {
    /// `None` if the clock could not be read at construction.
    start: Option<u64>,
}

impl ThreadCpu {
    fn start() -> Self {
        Self {
            start: thread_cpu_ns(),
        }
    }

    /// Milliseconds of thread CPU since construction.
    ///
    /// An unreadable clock reports `0` and says so in the log. `0` is a lie in
    /// the flattering direction, but the alternatives are worse: reporting the
    /// raw counter would charge the deploy for the thread's entire lifetime,
    /// and nothing downstream can act on the difference anyway — this number
    /// gates nothing.
    fn elapsed_ms(&self) -> u64 {
        match (self.start, thread_cpu_ns()) {
            (Some(started), Some(now)) => now.saturating_sub(started) / 1_000_000,
            _ => {
                tracing::warn!("CLOCK_THREAD_CPUTIME_ID unreadable; reporting init_cpu_ms = 0");
                0
            }
        }
    }
}

/// `CLOCK_THREAD_CPUTIME_ID` in nanoseconds, or `None` if it could not be
/// read. Mirrors `wisp_host::meters::try_read_thread_cpu_ns`; not shared with
/// it because the control plane does not depend on the host crate.
fn thread_cpu_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live, aligned `timespec` owned for the duration of the
    // call; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (rc == 0).then(|| (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64))
}

fn wizen(wasm: &[u8], init_func: &str, state: Arc<Mutex<Sandbox>>) -> anyhow::Result<Vec<u8>> {
    // Managed runtime startup can call WASI through the explicit PAL adapter.
    let wasi = wasi_imports(wasm);
    let mut w = Wizer::new();
    w.init_func(init_func);
    // Disable automatic WASI bindings; the explicit linker controls admission.
    w.allow_wasi(false)?;
    w.inherit_stdio(false);
    w.inherit_env(false);
    w.make_linker(Some(sealed_linker(state, wasi)))?;
    // The init entrypoint is stripped from the template: it has already run,
    // and leaving a second way to re-run static initialization inside a live
    // instance would be a way to desynchronize memory from the manifest.
    w.keep_init_func(false);
    // Match the engine profile (`validator::ENGINE_FEATURES`) as
    // closely as wizer's knobs allow. Modules using anything wider were
    // already rejected by the validator, which runs first.
    w.wasm_multi_memory(false);
    w.wasm_multi_value(true);
    w.wasm_bulk_memory(true);
    w.wasm_simd(true);
    w.wasm_reference_types(true);
    w.run(wasm)
}

/// Define allowed initialization bindings and record denied operations so
/// the guest can skip the recorded unit on a later attempt. Attempts restart
/// sandbox state; prior host effects are not rolled back.
fn sealed_linker(
    state: Arc<Mutex<Sandbox>>,
    wasi: Vec<WasiImport>,
) -> Rc<dyn Fn(&wt::Engine) -> anyhow::Result<wizer::Linker>> {
    Rc::new(move |engine: &wt::Engine| {
        let mut linker: wizer::Linker = wt::Linker::new(engine);
        install_pal_adapter(&mut linker, engine, &wasi, &state)?;
        for def in imports::IMPORTS {
            let st = state.clone();
            let ty = func_type(engine, def);
            let name = def.name;
            if def.init_visible || def.name == "entropy_fill" {
                install_init_visible(&mut linker, def, ty, st)?;
            } else {
                // A runtime-only capability defers its explicit unit; bring-up
                // mode can answer the coarse clock outside a unit.
                linker.func_new(
                    imports::HOST_MODULE,
                    name,
                    ty,
                    move |mut caller, args, rets| {
                        if st.lock().serves(name) {
                            return serve_pal(name, &st, &mut caller, args, rets);
                        }
                        let mut s = st.lock();
                        s.refusals += 1;
                        let unit = s.current_unit;
                        let told_to_skip = s.last_deferred;
                        if s.violation.is_none() {
                            s.violation = Some((unit.or(told_to_skip), name));
                        }
                        Err(anyhow::anyhow!(
                            "WISP init sandbox: capability `{}` is sealed during pre-init; \
                         the init unit that reached for it is deferred to lazy per-instance \
                         execution",
                            name
                        ))
                    },
                )?;
            }
        }
        Ok(linker)
    })
}

fn install_init_visible(
    linker: &mut wizer::Linker,
    def: &'static ImportDef,
    ty: wt::FuncType,
    st: Arc<Mutex<Sandbox>>,
) -> anyhow::Result<()> {
    let name = def.name;
    match name {
        "entropy_fill" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, rets| serve_pal(name, &st, &mut caller, args, rets),
            )?;
        }

        "init_hash_seed" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (ptr, len) = (args[0].unwrap_i32(), args[1].unwrap_i32());
                    let mut s = st.lock();
                    s.seed_reads += 1;
                    let seed = s.seed;
                    drop(s);
                    if len as usize != seed.len() {
                        anyhow::bail!(
                            "init_hash_seed expects a {}-byte buffer, guest passed {len}",
                            seed.len()
                        );
                    }
                    write_guest(&mut caller, ptr, &seed)
                },
            )?;
        }
        // Served only if the contract carries the import; the PoC's table may
        // not, in which case init-visible vars reach the version record but
        // not the guest (reported as a gap rather than faked).
        "init_config_get" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, rets| {
                    let (ptr, cap) = (args[0].unwrap_i32(), args[1].unwrap_i32());
                    let blob = st.lock().config.clone();
                    let n = (cap as usize).min(blob.len());
                    if n > 0 {
                        write_guest(&mut caller, ptr, &blob[..n])?;
                    }
                    rets[0] = wt::Val::I32(blob.len() as i32);
                    Ok(())
                },
            )?;
        }
        // The interposer. `begin` announces which unit is about to run so a
        // trap can be attributed; `deferred` is how the guest asks whether to
        // skip one on this pass.
        "init_unit_begin" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |_caller, args, _rets| {
                    let id = args[0].unwrap_i32() as u32;
                    let mut s = st.lock();
                    s.current_unit = if is_no_unit(id) { None } else { Some(id) };
                    s.last_deferred = None;
                    Ok(())
                },
            )?;
        }
        "init_unit_deferred" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |_caller, args, rets| {
                    let id = args[0].unwrap_i32() as u32;
                    let mut s = st.lock();
                    let skip = s.deferred.contains(&id);
                    if skip {
                        // The unit will not run, so a later trap is not its fault
                        // — unless the guest runs it anyway, which `last_deferred`
                        // is what catches.
                        s.current_unit = None;
                        s.last_deferred = Some(id);
                    }
                    rets[0] = wt::Val::I32(skip as i32);
                    Ok(())
                },
            )?;
        }
        // Structured logs from static initializers are captured and attached
        // to the initialization result.
        "log_emit" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (level, ptr, len) = (
                        args[0].unwrap_i32(),
                        args[1].unwrap_i32(),
                        args[2].unwrap_i32(),
                    );
                    let bytes = read_guest(&mut caller, ptr, len)?;
                    let mut s = st.lock();
                    if s.logs.len() < 1024 {
                        s.logs
                            .push(format!("[{level}] {}", String::from_utf8_lossy(&bytes)));
                    }
                    Ok(())
                },
            )?;
        }
        // A guest panic during init. Init-visible precisely so that a failed
        // pre-init reports something useful instead of `unreachable`.
        "abort" => {
            linker.func_new(
                imports::HOST_MODULE,
                name,
                ty,
                move |mut caller, args, _rets| {
                    let (ptr, len) = (args[0].unwrap_i32(), args[1].unwrap_i32());
                    let msg = read_guest(&mut caller, ptr, len)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_else(|_| "<unreadable>".into());
                    let unit = st.lock().current_unit;
                    anyhow::bail!(
                        "guest aborted during pre-init{}: {msg}",
                        unit.map(|u| format!(" (init unit {u})"))
                            .unwrap_or_default()
                    )
                },
            )?;
        }
        // A newly init-visible import the control plane has not been taught
        // about yet must NOT silently become a zero-returning stub — that is
        // the failure mode this whole file exists to avoid.
        other => {
            linker.func_new(imports::HOST_MODULE, other, ty, move |_c, _a, _r| {
                anyhow::bail!(
                    "import `{other}` is marked init_visible in wisp-abi but the init sandbox \
                     has no implementation for it; refusing to fake one"
                )
            })?;
        }
    }
    Ok(())
}

/// Serve initialization clock and entropy requests (see [`InitPal`]).
fn serve_pal(
    name: &'static str,
    st: &Arc<Mutex<Sandbox>>,
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    args: &[wt::Val],
    rets: &mut [wt::Val],
) -> anyhow::Result<()> {
    {
        let mut s = st.lock();
        *s.pal_calls.entry(name).or_default() += 1;
    }
    match name {
        // Runtime bring-up uses a fixed initialization clock sample.
        "clock_now_coarse" => {
            rets[0] = wt::Val::I64(0);
            Ok(())
        }
        "entropy_fill" => {
            let (ptr, len) = (args[0].unwrap_i32(), args[1].unwrap_i32());
            anyhow::ensure!(
                (0..=(1 << 20)).contains(&len),
                "entropy_fill length {len} out of range"
            );
            let bytes = host_entropy(st, len as usize);
            write_guest(caller, ptr, &bytes)
        }
        _ => anyhow::bail!("`{name}` is listed in RUNTIME_PAL but has no implementation"),
    }
}

/// Draws bytes from the host randomness source.
fn host_entropy(st: &Arc<Mutex<Sandbox>>, len: usize) -> Vec<u8> {
    st.lock().entropy_draws += 1;
    let mut out = vec![0u8; len];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut out);
    out
}

/// Record the denied platform capability and produce its trap.
fn refuse_capability(st: &Arc<Mutex<Sandbox>>, name: &'static str) -> anyhow::Error {
    let mut s = st.lock();
    s.refusals += 1;
    let unit = s.current_unit;
    let told_to_skip = s.last_deferred;
    if s.violation.is_none() {
        s.violation = Some((unit.or(told_to_skip), name));
    }
    anyhow::anyhow!(
        "WISP init sandbox: capability `{name}` is sealed during pre-init; the init \
         unit that reached for it is deferred to lazy per-instance execution"
    )
}

/// A non-`wisp` import as the module declares it: `(module, name, core type)`.
pub(crate) struct WasiImport {
    module: String,
    name: String,
    params: Vec<wasmparser::ValType>,
    results: Vec<wasmparser::ValType>,
}

/// Every non-`wisp` function import, with its core signature. The validator
/// has already refused anything outside the PAL whitelist, so this is a
/// re-read of bytes already judged — not a second judgement.
fn wasi_imports(wasm: &[u8]) -> Vec<WasiImport> {
    let mut types: Vec<(Vec<wasmparser::ValType>, Vec<wasmparser::ValType>)> = Vec::new();
    let mut out = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm).flatten() {
        match payload {
            wasmparser::Payload::TypeSection(r) => {
                for group in r.into_iter().flatten() {
                    for sub in group.into_types() {
                        match sub.composite_type.inner {
                            wasmparser::CompositeInnerType::Func(f) => {
                                types.push((f.params().to_vec(), f.results().to_vec()))
                            }
                            _ => types.push((Vec::new(), Vec::new())),
                        }
                    }
                }
            }
            wasmparser::Payload::ImportSection(r) => {
                for imp in r.into_imports().flatten() {
                    if imp.module == imports::HOST_MODULE {
                        continue;
                    }
                    let idx = match imp.ty {
                        wasmparser::TypeRef::Func(i) | wasmparser::TypeRef::FuncExact(i) => i,
                        _ => continue,
                    };
                    let Some((params, results)) = types.get(idx as usize).cloned() else {
                        continue;
                    };
                    out.push(WasiImport {
                        module: imp.module.to_string(),
                        name: imp.name.to_string(),
                        params,
                        results,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Apply the same capability policy to declared WASI adapter imports.
fn install_pal_adapter(
    linker: &mut wizer::Linker,
    engine: &wt::Engine,
    wasi: &[WasiImport],
    state: &Arc<Mutex<Sandbox>>,
) -> anyhow::Result<()> {
    use crate::pal::{self, Disposition};

    for imp in wasi {
        let cvt = |t: &wasmparser::ValType| match t {
            wasmparser::ValType::I64 => wt::ValType::I64,
            wasmparser::ValType::F32 => wt::ValType::F32,
            wasmparser::ValType::F64 => wt::ValType::F64,
            wasmparser::ValType::V128 => wt::ValType::V128,
            _ => wt::ValType::I32,
        };
        let ty = wt::FuncType::new(
            engine,
            imp.params.iter().map(cvt),
            imp.results.iter().map(cvt),
        );
        let st = state.clone();
        let Some(def) = pal::lookup(&imp.module, &imp.name) else {
            // Unreachable behind the validator, and defined anyway: leaving it
            // undefined would let wizer's dummy-import shim answer it with
            // zeros, which is the one outcome worse than a trap.
            let full = format!("{}::{}", imp.module, imp.name);
            linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, _r| {
                anyhow::bail!("`{full}` is outside the WASI PAL whitelist")
            })?;
            continue;
        };
        match (def.disposition, def.module, def.func) {
            // Initialization clock is zero, in nanoseconds. Same value the runtime
            // would get from `clock_now_coarse` here, widened by the same
            // quantum the runtime adapter uses.
            (Disposition::Delegate(_), m, "now") if m == pal::MONOTONIC_CLOCK => {
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, rets| {
                    // One lock, one snapshot. `parking_lot::Mutex` is not
                    // reentrant, so reading two fields through two `lock()`
                    // calls in one expression is a deadlock, not a style
                    // preference.
                    if !st.lock().serves("clock_now_coarse") {
                        return Err(refuse_capability(&st, "clock_now_coarse"));
                    }
                    *st.lock().pal_calls.entry("clock_now_coarse").or_default() += 1;
                    *st.lock().pal_calls.entry(pal::MONOTONIC_CLOCK).or_default() += 1;
                    rets[0] = wt::Val::I64(0);
                    Ok(())
                })?;
            }
            // A constant, and a truthful one. It reveals nothing about the
            // host, so it needs no scope rule.
            (Disposition::Delegate(_), m, "resolution") if m == pal::MONOTONIC_CLOCK => {
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, rets| {
                    rets[0] = wt::Val::I64(pal::CLOCK_QUANTUM_NS);
                    Ok(())
                })?;
            }
            // An empty environment has no external values to preserve, and
            // the value is the same on every replay — a template may bake it.
            (Disposition::Delegate(_), m, "get-environment") if m == pal::CLI_ENVIRONMENT => {
                linker.func_new(&imp.module, &imp.name, ty, move |mut c, args, _r| {
                    // Counted even though it is unconditional: this is the
                    // import that decided the question. `check.sh --wasi=trap`
                    // does not fail late, it fails *inside `_initialize`* on
                    // exactly this call, so a
                    // toolchain bump that stops reaching it — or starts
                    // reaching it more — should be visible as a number.
                    *st.lock().pal_calls.entry(pal::CLI_ENVIRONMENT).or_default() += 1;
                    let retptr = args
                        .last()
                        .and_then(|v| {
                            if let wt::Val::I32(p) = v {
                                Some(*p)
                            } else {
                                None
                            }
                        })
                        .ok_or_else(|| anyhow::anyhow!("get-environment has no return area"))?;
                    write_guest(&mut c, retptr, &[0u8; 8])
                })?;
            }
            // Delegated and canonical entropy use the same host randomness provider.
            (Disposition::Delegate(_), m, "get-random-bytes") if m == pal::RANDOM_RANDOM => {
                linker.func_new(&imp.module, &imp.name, ty, move |mut c, args, _r| {
                    if !st.lock().serves("entropy_fill") {
                        return Err(refuse_capability(&st, "entropy_fill"));
                    }
                    *st.lock().pal_calls.entry("entropy_fill").or_default() += 1;
                    *st.lock().pal_calls.entry(pal::RANDOM_RANDOM).or_default() += 1;
                    let len = match args.first() {
                        Some(wt::Val::I64(n)) if *n >= 0 => *n as usize,
                        _ => 0,
                    };
                    anyhow::ensure!(
                        len <= (1 << 20),
                        "entropy draw of {len} bytes is out of range"
                    );
                    let retptr = match args.get(1) {
                        Some(wt::Val::I32(p)) => *p,
                        _ => anyhow::bail!("get-random-bytes has no return area"),
                    };
                    let ptr = if len == 0 {
                        0
                    } else {
                        let realloc = c
                            .get_export("cabi_realloc")
                            .and_then(|e| e.into_func())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "guest declares `wasi:random` but exports no `cabi_realloc`"
                                )
                            })?
                            .typed::<(i32, i32, i32, i32), i32>(&c)?;
                        realloc.call(&mut c, (0, 0, 1, len as i32))?
                    };
                    let bytes = host_entropy(&st, len);
                    if len > 0 {
                        write_guest(&mut c, ptr, &bytes)?;
                    }
                    let mut area = [0u8; 8];
                    area[..4].copy_from_slice(&(ptr as u32).to_le_bytes());
                    area[4..].copy_from_slice(&(len as u32).to_le_bytes());
                    write_guest(&mut c, retptr, &area)
                })?;
            }
            // Everything else: the declaration links, the capability is absent.
            _ => {
                let full = format!("{}::{}", def.module, def.func);
                let why = def.why;
                linker.func_new(&imp.module, &imp.name, ty, move |_c, _a, _r| {
                    anyhow::bail!(
                        "guest called `{full}` during pre-init; the WISP PAL adapter satisfies \
                         it with a trap-on-call stub: {why}"
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn func_type(engine: &wt::Engine, def: &ImportDef) -> wt::FuncType {
    let cvt = |t: &Ty| match t {
        Ty::I32 => wt::ValType::I32,
        Ty::I64 => wt::ValType::I64,
    };
    wt::FuncType::new(
        engine,
        def.sig.params.iter().map(cvt),
        def.sig.results.iter().map(cvt),
    )
}

fn guest_memory(caller: &mut wt::Caller<'_, wizer::StoreData>) -> anyhow::Result<wt::Memory> {
    caller
        .get_export(imports::exports::MEMORY)
        .and_then(|e| e.into_memory())
        .ok_or_else(|| anyhow::anyhow!("guest exports no `memory`"))
}

fn write_guest(
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    ptr: i32,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mem = guest_memory(caller)?;
    mem.write(&mut *caller, ptr as usize, bytes)?;
    Ok(())
}

fn read_guest(
    caller: &mut wt::Caller<'_, wizer::StoreData>,
    ptr: i32,
    len: i32,
) -> anyhow::Result<Vec<u8>> {
    // A hostile module can pass any (ptr, len); the cap keeps a bogus length
    // from turning a log line into a 4 GiB allocation in the deploy worker.
    const MAX: i32 = 1 << 20;
    anyhow::ensure!((0..=MAX).contains(&len), "buffer length {len} out of range");
    let mem = guest_memory(caller)?;
    let mut buf = vec![0u8; len as usize];
    mem.read(&*caller, ptr as usize, &mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// cheap module facts (the validator has already parsed this module properly)
// ---------------------------------------------------------------------------

fn exports_func(wasm: &[u8], name: &str) -> bool {
    wasmparser::Parser::new(0).parse_all(wasm).any(|p| match p {
        Ok(wasmparser::Payload::ExportSection(r)) => {
            r.into_iter().filter_map(|e| e.ok()).any(|e| e.name == name)
        }
        _ => false,
    })
}

/// The declared ceiling on init-time linear memory. Module validation applies
/// its configured ceiling separately, so this is a second,
/// cheaper gate that runs before a single guest instruction executes.
fn declared_memory_bytes(wasm: &[u8]) -> u64 {
    let mut bytes = 0u64;
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        if let Ok(wasmparser::Payload::MemorySection(r)) = p {
            for m in r.into_iter().flatten() {
                bytes += m.maximum.unwrap_or(m.initial) * wisp_abi::WASM_PAGE_SIZE as u64;
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Burn roughly `ms` of CPU on this thread, by the same clock under test —
    /// so a broken clock cannot make this loop exit early and then pass the
    /// assertion it was supposed to fail.
    fn burn_cpu_ms(ms: u64) {
        let target = thread_cpu_ns().unwrap() + ms * 1_000_000;
        let mut x = 0u64;
        while thread_cpu_ns().unwrap() < target {
            for i in 0..10_000u64 {
                x = x.wrapping_add(i ^ x).rotate_left(7);
            }
        }
        std::hint::black_box(x);
    }

    /// The bug this instrument replaced, stated as a test: `init_cpu_ms` used
    /// to be `Instant::elapsed`, which would read ~200 here. A blocked thread
    /// consumes no CPU, and a deploy that waited is not a deploy that worked.
    #[test]
    fn time_spent_off_cpu_is_not_init_cpu() {
        let cpu = ThreadCpu::start();
        let wall = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let burned = cpu.elapsed_ms();
        assert!(
            wall.elapsed().as_millis() >= 150,
            "the window did not actually elapse"
        );
        assert!(
            burned < 20,
            "200 ms of sleep read as {burned} ms of thread CPU"
        );
    }

    /// ...and the other direction, or "measures nothing" would also pass.
    #[test]
    fn cpu_actually_burned_is_counted() {
        let cpu = ThreadCpu::start();
        burn_cpu_ms(100);
        let burned = cpu.elapsed_ms();
        assert!(
            burned >= 50,
            "100 ms of spinning read as {burned} ms of thread CPU"
        );
    }

    /// A clock that could not be read must report zero, not the thread's whole
    /// CPU lifetime — the difference between an unmeasured deploy and one that
    /// appears to have cost minutes.
    #[test]
    fn an_unreadable_clock_reports_zero_not_the_threads_lifetime() {
        burn_cpu_ms(20);
        assert!(
            thread_cpu_ns().unwrap() > 20_000_000,
            "this thread has burned real CPU"
        );
        assert_eq!(ThreadCpu { start: None }.elapsed_ms(), 0);
    }

    /// A module whose `wisp_initialize` burns a visible amount of CPU in a
    /// loop the compiler cannot delete — the store address moves, so it is not
    /// a dead store. Kept small enough to be a unit test and large enough that
    /// millisecond granularity is not the thing under test.
    fn cpu_burning_module() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "wisp_initialize")
                   (local $i i32)
                   (local.set $i (i32.const 250000000))
                   (block $done
                     (loop $l
                       (br_if $done (i32.eqz (local.get $i)))
                       (i32.store
                         (i32.and (local.get $i) (i32.const 0xffc))
                         (local.get $i))
                       (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                       (br $l)))))"#,
        )
        .expect("the fixture must assemble")
    }

    fn req_for(wasm: &[u8]) -> PreInitRequest {
        PreInitRequest {
            module_hash: ModuleHash::of(wasm),
            init_blob: Vec::new(),
        }
    }

    /// The mechanism, asserted rather than assumed: `install` makes rayon's
    /// *current* pool the scoped one, which is what nested parallel work —
    /// wasmtime's compile — resolves against.
    ///
    /// The second half keeps this from being vacuous on a one-core machine,
    /// where the global pool is also one thread and a no-op `install` would
    /// pass the first assertion.
    #[test]
    fn the_compile_pool_narrows_rayons_current_pool() {
        let pool = compile_pool().expect("a one-thread pool must build");
        let inside = pool.install(rayon::current_num_threads);
        assert_eq!(
            inside, PREINIT_COMPILE_THREADS,
            "`install` did not take effect"
        );

        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        if cores > PREINIT_COMPILE_THREADS {
            let outside = rayon::current_num_threads();
            assert!(
                outside > inside,
                "on a {cores}-core machine the global pool is {outside} threads and the                  scoped pool {inside}; equal would mean `install` narrowed nothing"
            );
        }
    }

    /// Scoping the compile must not change the artifact. Both arms of `run`'s
    /// match are exercised, because the `None` arm — taken when a pool cannot
    /// be built — is otherwise unreachable and would rot unnoticed.
    #[test]
    fn the_pooled_and_unpooled_paths_produce_the_same_template() {
        let wasm = cpu_burning_module();
        let req = req_for(&wasm);
        let pre = PreInitializer::new();

        let pooled = pre.run(&wasm, &req).expect("pre-init must converge");
        let unpooled = pre
            .replay(&wasm, &req)
            .expect("the fallback path must converge");

        assert_eq!(pooled.template_wasm, unpooled.template_wasm);
        assert_eq!(pooled.passes, 1);
        assert_eq!(unpooled.passes, 1);
    }

    /// The bug the `run`/`replay` split exists to prevent: `install` parks the
    /// calling thread and runs the closure on a pool worker, so a `ThreadCpu`
    /// started outside the scope measures a thread that is asleep and reports
    /// `0` for every deploy — silently, on a number nothing else checks.
    #[test]
    fn init_cpu_ms_measures_the_thread_that_does_the_work() {
        let wasm = cpu_burning_module();
        let wall = std::time::Instant::now();
        let out = PreInitializer::new()
            .run(&wasm, &req_for(&wasm))
            .expect("must converge");
        let wall_ms = wall.elapsed().as_millis() as u64;

        assert!(
            wall_ms >= 20,
            "the fixture must take long enough to measure; took {wall_ms} ms"
        );
        assert!(
            out.init_cpu_ms * 2 >= wall_ms,
            "pre-init took {wall_ms} ms of wall clock but reported {} ms of CPU;              the clock is reading a thread that is not doing the work",
            out.init_cpu_ms
        );
    }
}
