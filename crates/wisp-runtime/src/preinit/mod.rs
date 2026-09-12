//! Deploy-time pre-initialization: the module is instantiated once in a
//! sealed sandbox, its init export runs, and the resulting memory, globals
//! and tables become the template.
//!
//! An init unit that reaches for a sealed capability traps; the run is then
//! replayed from scratch with that unit deferred, until a pass completes.
//! Each retry starts with a fresh sandbox. Initialization configuration is
//! retained while initialization randomness is renewed.

mod filesystem;
mod sandbox;
mod section;
mod thread_cpu;
mod wasi;

use std::collections::BTreeSet;
use std::sync::Arc;

use parking_lot::Mutex;
use wisp_abi::config::InitSandbox;
use wisp_abi::imports;
use wisp_engine::{ModuleHash, PlatformFiles};
use wizer::wasmtime as wt;
use wizer::Wizer;

use sandbox::Sandbox;
use thread_cpu::ThreadCpu;

/// Declared linear-memory ceiling checked before initialization.
const INIT_MEMORY_LIMIT_BYTES: u64 = 512 << 20;

/// Maximum number of initialization attempts.
const MAX_PASSES: u32 = 16;

/// Threads wizer's embedded wasmtime compiles on. One: sweeping 1, 2, 4, 8
/// and 16 over five guests from 55 KB to 15 MB moved wall time by less than
/// the run-to-run noise while CPU rose with the count, and the global pool
/// (256 workers on that machine) cost 1.6 to 2.0 times the wall time of one
/// worker. One worker also keeps every compile cycle on the thread
/// `init_cpu_ms` is read from.
const PREINIT_COMPILE_THREADS: usize = 1;

/// The host treats 0 and `u32::MAX` as no active initialization unit.
const NO_UNIT: u32 = u32::MAX;
const NO_UNIT_ALT: u32 = 0;

fn is_no_unit(id: u32) -> bool {
    id == NO_UNIT || id == NO_UNIT_ALT
}

/// Runtime PAL names considered by the additional capability policy.
/// Application entropy is admitted separately from this mode and scope list.
pub const RUNTIME_PAL: &[&str] = &["clock_now_coarse", "entropy_fill"];

fn serves(pal: InitSandbox, name: &str) -> bool {
    pal == InitSandbox::RuntimeBringUp && RUNTIME_PAL.contains(&name)
}

#[derive(Default)]
pub struct PreInitializer {
    pal: InitSandbox,
    platform_files: PlatformFiles,
}

#[derive(Debug, Clone)]
pub struct PreInitRequest {
    pub module_hash: ModuleHash,
    /// Serialized configuration available to initialization.
    pub init_blob: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PreInitOutcome {
    /// The module with its data segments replaced by the post-init image and
    /// the init export removed.
    pub template_wasm: Vec<u8>,
    /// Unit IDs recorded for the guest to skip on subsequent attempts.
    pub deferred_units: Vec<u32>,
    pub passes: u32,
    /// CPU of the thread that ran the replay loop, in milliseconds. Zero
    /// when the clock could not be read.
    pub init_cpu_ms: u64,
    pub template_bytes: u64,
    /// Digest of explicit initialization inputs; fresh random output is excluded.
    pub reproducible_digest: [u8; 32],
    /// The capability each deferred unit reached for.
    pub deferred_reasons: Vec<(u32, &'static str)>,
    /// `log_emit` output from the final pass.
    pub init_logs: Vec<String>,
    /// Capability refusals over all passes.
    pub capability_refusals: u32,
    /// PAL call counts from the successful initialization pass.
    pub pal_calls: Vec<(&'static str, u32)>,
    pub seed_reads: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PreInitError {
    /// Native compiler workers could not be created.
    #[error("creating pre-initialisation compiler workers: {0}")]
    CompilerWorkers(#[from] rayon::ThreadPoolBuildError),
    #[error("module does not export `{0}`; nothing to pre-initialize")]
    NoInitFunc(&'static str),
    /// Nothing to defer: deferring a guess would change program behaviour.
    #[error(
        "pre-init trapped outside any init unit (`{import}`); wrap initialization that may \
         touch host capabilities in an init unit: {cause}"
    )]
    Unattributable {
        import: &'static str,
        cause: anyhow::Error,
    },
    #[error(
        "init unit {unit} trapped on `{import}` again after being deferred; the guest is \
         ignoring `init_unit_deferred`"
    )]
    DeferralIgnored { unit: u32, import: &'static str },
    #[error("pre-init did not converge in {0} passes")]
    NoConvergence(u32),
    #[error("declared init memory of {bytes} bytes exceeds the init limit of {limit} bytes")]
    MemoryLimit { bytes: u64, limit: u64 },
    /// A guest `abort`, an `unreachable`, an out-of-memory: not a sealed
    /// capability, so nothing to defer.
    #[error("pre-init trapped: {0}")]
    Trapped(anyhow::Error),
}

impl PreInitializer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_pal(mut self, pal: InitSandbox) -> Self {
        self.pal = pal;
        self
    }

    /// Declare the read-only files the PAL filesystem serves during
    /// pre-initialisation: the same set the execution engine serves, so a
    /// static initialiser that reads one bakes the same bytes a dispatch
    /// would see.
    pub fn with_platform_files(mut self, files: PlatformFiles) -> Self {
        self.platform_files = files;
        self
    }

    /// Run `wisp_initialize`, recording denied capabilities for the guest's unit protocol.
    pub fn run(&self, wasm: &[u8], req: &PreInitRequest) -> Result<PreInitOutcome, PreInitError> {
        if !exports_func(wasm, imports::exports::INITIALIZE) {
            return Err(PreInitError::NoInitFunc(imports::exports::INITIALIZE));
        }
        let declared = declared_memory_bytes(wasm);
        if declared > INIT_MEMORY_LIMIT_BYTES {
            return Err(PreInitError::MemoryLimit {
                bytes: declared,
                limit: INIT_MEMORY_LIMIT_BYTES,
            });
        }
        let pool = compile_pool()?;
        pool.install(|| self.replay(wasm, req))
    }

    /// Runs on the thread that does the work, so the CPU clock started here
    /// reads that thread and not the caller parked outside `install`.
    fn replay(&self, wasm: &[u8], req: &PreInitRequest) -> Result<PreInitOutcome, PreInitError> {
        let mut deferred: BTreeSet<u32> = BTreeSet::new();
        let mut reasons: Vec<(u32, &'static str)> = Vec::new();
        let mut refusals = 0u32;
        let cpu = ThreadCpu::start();

        for pass in 1..=MAX_PASSES {
            let state = Arc::new(Mutex::new(Sandbox::new(
                req.init_blob.clone(),
                deferred.clone(),
                self.pal,
                self.platform_files.clone(),
            )));

            let result = wizen(wasm, state.clone());
            let s = state.lock();
            refusals += s.refusals;

            match result {
                Ok(mut template_wasm) => {
                    section::append_deferred_init_section(&mut template_wasm, &deferred);
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
                    Some((Some(unit), import)) if unit != NO_UNIT => {
                        if !deferred.insert(unit) {
                            return Err(PreInitError::DeferralIgnored { unit, import });
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
                        return Err(PreInitError::Unattributable { import, cause: e })
                    }
                    None => return Err(PreInitError::Trapped(e)),
                },
            }
        }
        Err(PreInitError::NoConvergence(MAX_PASSES))
    }
}

/// A deploy-scoped pool keeps preparation CPU on its measured worker.
fn compile_pool() -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(PREINIT_COMPILE_THREADS)
        .stack_size(wisp_engine::COMPILER_STACK_BYTES)
        .thread_name(|i| format!("wisp-preinit-{i}"))
        .build()
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

fn wizen(wasm: &[u8], state: Arc<Mutex<Sandbox>>) -> anyhow::Result<Vec<u8>> {
    let wasi = wasi::wasi_imports(wasm);
    let mut w = Wizer::new();
    w.init_func(imports::exports::INITIALIZE);
    w.allow_wasi(false)?;
    w.inherit_stdio(false);
    w.inherit_env(false);
    w.make_linker(Some(sandbox::sealed_linker(state, wasi)))?;
    // A template with the init export left in would be a way to re-run
    // static initialization inside a live instance.
    w.keep_init_func(false);
    w.wasm_multi_memory(false);
    w.wasm_multi_value(true);
    w.wasm_bulk_memory(true);
    w.wasm_simd(true);
    w.wasm_reference_types(true);
    w.run(wasm)
}

fn exports_func(wasm: &[u8], name: &str) -> bool {
    wasmparser::Parser::new(0).parse_all(wasm).any(|p| match p {
        Ok(wasmparser::Payload::ExportSection(r)) => {
            r.into_iter().filter_map(|e| e.ok()).any(|e| e.name == name)
        }
        _ => false,
    })
}

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

fn func_type(engine: &wt::Engine, def: &imports::ImportDef) -> wt::FuncType {
    let cvt = |t: &imports::Ty| match t {
        imports::Ty::I32 => wt::ValType::I32,
        imports::Ty::I64 => wt::ValType::I64,
    };
    wt::FuncType::new(
        engine,
        def.sig.params.iter().map(cvt),
        def.sig.results.iter().map(cvt),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looping_module() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "wisp_initialize")
                   (local $i i32)
                   (local.set $i (i32.const 250000))
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

    /// The second half keeps this from being vacuous on a one-core machine,
    /// where the global pool is also one thread.
    #[test]
    fn the_compile_pool_narrows_rayons_current_pool() {
        let pool = compile_pool().expect("a one-thread pool must build");
        let inside = pool.install(rayon::current_num_threads);
        assert_eq!(
            inside, PREINIT_COMPILE_THREADS,
            "`install` did not make the scoped pool current"
        );

        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        if cores > PREINIT_COMPILE_THREADS {
            let outside = rayon::current_num_threads();
            assert!(
                outside > inside,
                "on a {cores}-core machine the global pool is {outside} threads and the scoped \
                 pool {inside}; equal would mean `install` narrowed nothing"
            );
        }
    }

    #[test]
    fn the_pooled_and_unpooled_paths_produce_the_same_template() {
        let wasm = looping_module();
        let req = req_for(&wasm);
        let pre = PreInitializer::new();

        let pooled = pre.run(&wasm, &req).expect("pre-init must converge");
        let unpooled = pre
            .replay(&wasm, &req)
            .expect("the fallback path must converge");

        assert_eq!(
            pooled.template_wasm, unpooled.template_wasm,
            "scoping the compile must not change the artifact"
        );
        assert_eq!(pooled.passes, 1, "a pure module converges in one pass");
    }
}
