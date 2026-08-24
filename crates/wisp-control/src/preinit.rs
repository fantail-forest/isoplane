//! Initialization inputs and captured state.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::imports::{self, ImportDef, Ty};
use wisp_engine::ModuleHash;
use wizer::wasmtime as wt;
use wizer::Wizer;

/// Initialization time and declared-memory limits.
pub use crate::validator::{INIT_CPU_LIMIT_MS, INIT_MEMORY_LIMIT_BYTES};

/// Maximum number of initialization attempts.
pub const MAX_PASSES: u32 = 16;

/// Reserved init-unit id meaning "no unit is in scope" (see the protocol in
/// [`PreInitializer::run`]). A trap while no unit is in scope is
/// unattributable and fails the deploy rather than deferring the wrong thing.
pub const NO_UNIT: u32 = 0;

pub struct PreInitializer {
    cpu_limit_ms: u64,
    memory_limit_bytes: u64,
    max_passes: u32,
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
    /// Hash-seed reads on the successful initialization pass.
    pub seed_reads: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PreInitError {
    #[error("module does not export `{0}`; nothing to pre-initialize")]
    NoInitFunc(&'static str),
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
        "pre-init exceeded the elapsed-time limit of {limit_ms} ms (ran {ran_ms} ms); \
             checked after initialization returned"
    )]
    CpuLimit { limit_ms: u64, ran_ms: u64 },
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
            cpu_limit_ms: INIT_CPU_LIMIT_MS,
            memory_limit_bytes: INIT_MEMORY_LIMIT_BYTES,
            max_passes: MAX_PASSES,
        }
    }

    /// Set the elapsed-time and declared-memory limits.
    pub fn with_limits(cpu_limit_ms: u64, memory_limit_bytes: u64) -> Self {
        Self {
            cpu_limit_ms,
            memory_limit_bytes,
            max_passes: MAX_PASSES,
        }
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
    ///   init_unit_begin(0)                   ; optional: leave unit scope
    /// ```
    ///
    /// Id 0 is reserved for "no unit in scope". A guest that runs code
    /// between units which might touch a capability should bracket it with
    /// `init_unit_begin(0)`, because a trap the host cannot attribute fails
    /// the deploy rather than deferring a guess.
    pub fn run(&self, wasm: &[u8], req: &PreInitRequest) -> crate::Result<PreInitOutcome> {
        if !exports_init_func(wasm) {
            return Err(PreInitError::NoInitFunc(imports::exports::INITIALIZE).into());
        }
        let declared = declared_memory_bytes(wasm);
        if declared > self.memory_limit_bytes {
            return Err(PreInitError::MemoryLimit {
                bytes: declared,
                limit: self.memory_limit_bytes,
            }
            .into());
        }

        let mut deferred: BTreeSet<u32> = BTreeSet::new();
        let mut reasons: Vec<(u32, &'static str)> = Vec::new();
        let mut refusals = 0u32;
        let started = std::time::Instant::now();

        for pass in 1..=self.max_passes {
            let state = Arc::new(Mutex::new(Sandbox {
                seed: rand::random(),
                config: req.init_blob.clone(),
                deferred: deferred.clone(),
                ..Default::default()
            }));

            let result = wizen(wasm, state.clone());
            let s = state.lock();
            refusals += s.refusals;

            match result {
                Ok(template_wasm) => {
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    // This rejects a slow result after return; it cannot interrupt Wizer.
                    if elapsed_ms > self.cpu_limit_ms {
                        return Err(PreInitError::CpuLimit {
                            limit_ms: self.cpu_limit_ms,
                            ran_ms: elapsed_ms,
                        }
                        .into());
                    }
                    tracing::info!(
                        module = %req.module_hash,
                        passes = pass,
                        deferred = deferred.len(),
                        bytes = template_wasm.len(),
                        "pre-init converged"
                    );
                    return Ok(PreInitOutcome {
                        reproducible_digest: reproducible_digest(wasm, req),
                        template_bytes: template_wasm.len() as u64,
                        template_wasm,
                        deferred_units: deferred.iter().copied().collect(),
                        passes: pass,
                        init_cpu_ms: elapsed_ms,
                        deferred_reasons: reasons,
                        init_logs: s.logs.clone(),
                        capability_refusals: refusals,
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
}

fn wizen(wasm: &[u8], state: Arc<Mutex<Sandbox>>) -> anyhow::Result<Vec<u8>> {
    let mut w = Wizer::new();
    w.init_func(imports::exports::INITIALIZE);
    // Disable automatic WASI bindings; the explicit linker controls admission.
    w.allow_wasi(false)?;
    w.inherit_stdio(false);
    w.inherit_env(false);
    w.make_linker(Some(sealed_linker(state)))?;
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
) -> Rc<dyn Fn(&wt::Engine) -> anyhow::Result<wizer::Linker>> {
    Rc::new(move |engine: &wt::Engine| {
        let mut linker: wizer::Linker = wt::Linker::new(engine);
        for def in imports::IMPORTS {
            let st = state.clone();
            let ty = func_type(engine, def);
            let name = def.name;
            if def.init_visible || def.name == "entropy_fill" {
                install_init_visible(&mut linker, def, ty, st)?;
            } else {
                linker.func_new(
                    imports::HOST_MODULE,
                    name,
                    ty,
                    move |_caller, _args, _rets| {
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
                move |mut caller, args, _rets| {
                    let [wt::Val::I32(ptr), wt::Val::I32(len)] = args else {
                        anyhow::bail!("entropy_fill requires an output pointer and length");
                    };
                    anyhow::ensure!(
                        (0..=(1 << 20)).contains(len),
                        "entropy_fill length {len} out of range"
                    );
                    let mut bytes = vec![0u8; *len as usize];
                    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
                    write_guest(&mut caller, *ptr, &bytes)
                },
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
                    s.current_unit = if id == NO_UNIT { None } else { Some(id) };
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

fn exports_init_func(wasm: &[u8]) -> bool {
    wasmparser::Parser::new(0).parse_all(wasm).any(|p| match p {
        Ok(wasmparser::Payload::ExportSection(r)) => r
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.name == imports::exports::INITIALIZE),
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
