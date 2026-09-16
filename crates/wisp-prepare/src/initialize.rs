//! Runs the initialization contract through the same engine used by workers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use wisp_abi::config::InitSandbox;
use wisp_engine::{Completion, CpuBudget, ImportCall, InstanceOps, WispEngine};

use crate::{bake, PrepareError};

/// Explicit deployment configuration and capability limits supplied by preparation.
#[derive(Clone, Serialize, Deserialize)]
pub struct Initialization {
    /// Explicit outbound authority; absent contexts cannot create host I/O.
    pub network: Option<crate::InitializationNetwork>,
    /// Serialized deployment configuration.
    pub config: Vec<u8>,
    /// Deployment-bound JSON settings, including secrets; never include in diagnostics.
    pub runtime_config: Vec<u8>,
    /// Policy selected by the caller; it is never widened by preparation.
    pub sandbox: InitSandbox,
    /// Total initialization CPU budget across replay passes.
    pub cpu_ms: u32,
    /// Total initialization wall backstop, including asynchronous waits and replay passes.
    pub wall_ms: u32,
    /// Maximum replay passes before refusal.
    pub max_passes: u32,
    /// Maximum retained guest log bytes across a successful pass.
    pub log_bytes: usize,
}

impl Initialization {
    /// Digest of explicit initialization inputs; fresh random output is excluded.
    pub fn digest(&self, input_hash: [u8; 32]) -> Result<[u8; 32], postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"wisp.initialization-input.v2\0");
        hash.update(&postcard::to_allocvec(&(
            input_hash,
            &self.config,
            &self.runtime_config,
            self.sandbox,
            &self.network,
        ))?);
        Ok(*hash.finalize().as_bytes())
    }
}

/// Initialized portable bytes and explicit replay state, ready for trusted compilation.
#[derive(Serialize, Deserialize)]
pub struct Initialized {
    /// Portable initialized module; its init export is removed.
    pub wasm: Vec<u8>,
    /// Units deferred because they requested forbidden capabilities.
    pub deferred: Vec<u32>,
    /// CPU charged by the shared engine across all passes.
    pub cpu_ns: u64,
    /// CPU of the preparation thread across compilation, replay and state verification.
    pub preparation_cpu_ns: u64,
    /// The capability each deferred unit attempted.
    pub deferred_reasons: Vec<(u32, String)>,
    /// Forbidden capability attempts across replay passes.
    pub capability_refusals: u32,
    /// Runtime capabilities answered on the successful pass.
    pub pal_calls: Vec<(String, u32)>,
    /// Seed reads on the successful pass.
    pub seed_reads: u32,
    /// Number of passes executed.
    pub passes: u32,
    /// Bounded log output from the successful pass.
    pub logs: Vec<String>,
}

/// Execute a validated application under the supplied worker engine in a preparation process.
/// Fails if initialization traps, exceeds a limit, or creates state the schema cannot preserve.
pub fn initialize(
    engine: &WispEngine,
    wasm: &[u8],
    request: &Initialization,
) -> Result<Initialized, PrepareError> {
    initialize_with_cache(
        engine,
        wasm,
        request,
        None,
        &wisp_engine::PlatformFiles::default(),
        None,
        CaptureMode::Portable,
    )
    .map(|output| output.initialized)
}

#[derive(Clone, Copy)]
pub(crate) enum CaptureMode {
    Portable,
    #[cfg(target_os = "linux")]
    Layered(wisp_engine::prepared::PreparedIdentity),
}

pub(crate) struct InitializationOutput {
    pub initialized: Initialized,
    #[cfg(target_os = "linux")]
    pub prepared: Option<wisp_engine::prepared::PreparedTemplate>,
}

pub(crate) fn initialize_with_cache(
    engine: &WispEngine,
    wasm: &[u8],
    request: &Initialization,
    cache: Option<&wisp_engine::CompilationCache>,
    application_files: &wisp_engine::PlatformFiles,
    memory_limit_bytes: Option<u64>,
    capture: CaptureMode,
) -> Result<InitializationOutput, PrepareError> {
    if request.runtime_config.len() > wisp_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES
        || serde_json::from_slice::<BTreeMap<String, String>>(&request.runtime_config).is_err()
    {
        return Err(PrepareError::InvalidRequest(
            "deployment settings must be a bounded JSON object of strings",
        ));
    }
    if request.cpu_ms == 0 || request.wall_ms == 0 || request.max_passes == 0 {
        return Err(PrepareError::InvalidRequest(
            "initialization limits must be positive",
        ));
    }
    let cpu_start = preparation_cpu()?;
    let mut deferred_reasons = Vec::new();
    let mut capability_refusals = 0;
    let instrumented = bake::instrument(wasm).map_err(PrepareError::State)?;
    let template = match cache {
        Some(cache) => cache
            .load(engine, &instrumented.wasm)
            .map(|(template, _compiled)| template),
        None => match capture {
            CaptureMode::Portable => engine.load_template(&instrumented.wasm),
            #[cfg(target_os = "linux")]
            CaptureMode::Layered(_) => engine.load_layer_source(&instrumented.wasm),
        },
    }
    .and_then(|template| template.with_application_files(application_files.clone()))
    .map_err(PrepareError::State)?;
    let reactor = request
        .network
        .as_ref()
        .map(|_| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
        })
        .transpose()
        .map_err(PrepareError::Io)?;
    let initialization_start = Instant::now();
    let mut deferred = BTreeSet::new();
    let mut cpu_ns = 0;
    for pass in 1..=request.max_passes {
        let io = match (&request.network, &reactor) {
            (Some(network), Some(reactor)) => {
                let _entered = reactor.enter();
                Some(Arc::new(network.start(reactor.handle().clone())?))
            }
            _ => None,
        };
        let state = Arc::new(Mutex::new(Sandbox::new(request, deferred.clone())));
        let mut instance = engine
            .activate_initialization(
                &template,
                Box::new(Bindings {
                    state: state.clone(),
                    io: io.clone(),
                    memory_limit_bytes,
                }),
            )
            .map_err(PrepareError::State)?;
        if template.has_initialize() {
            let mut outcome = instance.run_initialize(initialization_budget(
                request,
                cpu_ns,
                initialization_start.elapsed(),
            )?);
            cpu_ns = cpu_ns.saturating_add(outcome.cpu_ns);
            while outcome.completion == Completion::Parked {
                let (Some(io), Some(reactor)) = (&io, &reactor) else {
                    return Err(PrepareError::InvalidRequest(
                        "initializer is pending without an asynchronous host context",
                    ));
                };
                let remaining = Duration::from_millis(u64::from(request.wall_ms))
                    .checked_sub(initialization_start.elapsed())
                    .ok_or(PrepareError::Limit("initialization wall time"))?;
                let completion = reactor
                    .block_on(async { tokio::time::timeout(remaining, io.next_completion()).await })
                    .map_err(|_| PrepareError::Limit("initialization wall time"))?
                    .ok_or(PrepareError::InvalidRequest(
                        "initializer is pending without a host wake source",
                    ))?;
                outcome = instance.wake(
                    completion.0,
                    &completion.1,
                    initialization_budget(request, cpu_ns, initialization_start.elapsed())?,
                );
                cpu_ns = cpu_ns.saturating_add(outcome.cpu_ns);
            }
            initialization_budget(request, cpu_ns, initialization_start.elapsed())?;
            if outcome.completion != Completion::Done {
                if let Some(import) = instance.last_capability_violation() {
                    capability_refusals += 1;
                    let unit = {
                        let state = state.lock();
                        state.unit.or(state.last_deferred)
                    };
                    if let Some(unit) = unit {
                        if !deferred.insert(unit) {
                            return Err(PrepareError::DeferralIgnored {
                                unit,
                                import: import.into(),
                            });
                        }
                        deferred_reasons.push((unit, import.to_string()));
                        continue;
                    }
                    return Err(PrepareError::Unattributable {
                        completion: outcome.completion,
                        import: import.to_string(),
                        detail: outcome
                            .trap
                            .map(|trap| trap.message)
                            .unwrap_or_else(|| "forbidden initialization capability".into()),
                    });
                }
                return Err(PrepareError::Initialization {
                    completion: outcome.completion,
                    detail: outcome
                        .trap
                        .map(|trap| trap.message)
                        .unwrap_or_else(|| "initializer did not return successfully".into()),
                });
            }
        } else {
            #[cfg(target_os = "linux")]
            let prepared = match capture {
                CaptureMode::Portable => None,
                CaptureMode::Layered(identity) => Some(
                    engine
                        .prepare_template_with_files(wasm, identity, application_files.clone())
                        .map_err(|error| PrepareError::State(error.into()))?,
                ),
            };
            return Ok(InitializationOutput {
                #[cfg(target_os = "linux")]
                prepared,
                initialized: Initialized {
                    wasm: wasm.to_vec(),
                    deferred: Vec::new(),
                    cpu_ns,
                    preparation_cpu_ns: preparation_cpu()?.saturating_sub(cpu_start),
                    deferred_reasons,
                    capability_refusals,
                    pal_calls: instance
                        .initialization_pal_calls()
                        .map_err(PrepareError::State)?,
                    seed_reads: state.lock().seed_reads,
                    passes: 0,
                    logs: Vec::new(),
                },
            });
        }
        let prepared_resources = io
            .as_ref()
            .map(|io| io.finish())
            .transpose()
            .map_err(|error| {
                PrepareError::State(anyhow::anyhow!(
                    "initialization retained native state: {error}"
                ))
            })?
            .unwrap_or_default();
        let deferred: Vec<_> = deferred.into_iter().collect();
        #[cfg(target_os = "linux")]
        let prepared = match capture {
            CaptureMode::Portable => None,
            CaptureMode::Layered(identity) => Some(
                engine
                    .prepare_initialized_template(
                        &template,
                        &mut instance,
                        identity,
                        prepared_resources.clone(),
                        deferred.clone(),
                    )
                    .map_err(|error| PrepareError::State(error.into()))?,
            ),
        };
        let captured = instance
            .capture_mutable_state()
            .map_err(PrepareError::State)?;
        let globals = captured_globals(&instrumented, &captured.globals)?;
        let mut output = bake::bake(&instrumented.wasm, instance.memory(), &globals, &deferred)
            .map_err(PrepareError::State)?;
        let resources =
            postcard::to_allocvec(&prepared_resources).map_err(PrepareError::Encoding)?;
        wasm_encoder::Section::append_to(
            &wasm_encoder::CustomSection {
                name: wisp_abi::native_io::PREPARED_RESOURCES_SECTION.into(),
                data: resources.into(),
            },
            &mut output,
        );
        // Reinstantiation verifies captured state without invoking the initializer again.
        let restored_template = match capture {
            CaptureMode::Portable => engine
                .load_template(&output)
                .and_then(|template| template.with_application_files(application_files.clone()))
                .map_err(PrepareError::State)?,
            #[cfg(target_os = "linux")]
            CaptureMode::Layered(_) => unsafe {
                // SAFETY: this engine captured `prepared` above; its native bytes came from that compiler.
                engine.load_prepared_product(
                    prepared
                        .as_ref()
                        .ok_or(PrepareError::InvalidRequest("missing initialized layer"))?,
                )
            }
            .map_err(|error| PrepareError::State(error.into()))?,
        };
        let mut restored = engine
            .activate_initialization(
                &restored_template,
                Box::new(Bindings {
                    state: state.clone(),
                    io: io.clone(),
                    memory_limit_bytes,
                }),
            )
            .map_err(PrepareError::State)?;
        if restored.memory() != instance.memory() {
            return Err(PrepareError::InvalidRequest(
                "captured initial memory differs from initialized memory",
            ));
        }
        let restored_state = restored
            .capture_mutable_state()
            .map_err(PrepareError::State)?;
        if postcard::to_allocvec(&captured).map_err(PrepareError::Encoding)?
            != postcard::to_allocvec(&restored_state).map_err(PrepareError::Encoding)?
        {
            return Err(PrepareError::InvalidRequest(
                "captured mutable state differs from initialized state",
            ));
        }
        let pal_calls = instance
            .initialization_pal_calls()
            .map_err(PrepareError::State)?;
        let state = state.lock();
        let logs = state.logs.clone();
        return Ok(InitializationOutput {
            #[cfg(target_os = "linux")]
            prepared,
            initialized: Initialized {
                wasm: output,
                deferred,
                cpu_ns,
                preparation_cpu_ns: preparation_cpu()?.saturating_sub(cpu_start),
                deferred_reasons,
                capability_refusals,
                pal_calls,
                seed_reads: state.seed_reads,
                passes: pass,
                logs,
            },
        });
    }
    Err(PrepareError::NoConvergence(request.max_passes))
}

fn captured_globals(
    instrumented: &bake::Instrumented,
    values: &[wisp_engine::snapshot::GlobalValue],
) -> Result<BTreeMap<u32, wisp_engine::snapshot::GlobalValue>, PrepareError> {
    let mut indexes = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(&instrumented.wasm) {
        if let wasmparser::Payload::ExportSection(reader) =
            payload.map_err(|error| PrepareError::State(error.into()))?
        {
            for export in reader {
                let export = export.map_err(|error| PrepareError::State(error.into()))?;
                if export.kind == wasmparser::ExternalKind::Global
                    && instrumented
                        .globals
                        .iter()
                        .any(|(index, _)| *index == export.index)
                {
                    indexes.push(export.index);
                }
            }
        }
    }
    indexes.sort();
    if indexes.len() != values.len() {
        return Err(PrepareError::InvalidRequest(
            "captured globals do not match instrumented exports",
        ));
    }
    Ok(indexes.into_iter().zip(values.iter().copied()).collect())
}

struct Sandbox {
    seed: Option<[u8; 16]>,
    config: Vec<u8>,
    runtime_config: Vec<u8>,
    deferred: BTreeSet<u32>,
    unit: Option<u32>,
    last_deferred: Option<u32>,
    bringup: bool,
    draws: u64,
    logs: Vec<String>,
    remaining_log_bytes: usize,
    seed_reads: u32,
}

impl Sandbox {
    fn new(request: &Initialization, deferred: BTreeSet<u32>) -> Self {
        Self {
            seed: None,
            config: request.config.clone(),
            runtime_config: request.runtime_config.clone(),
            deferred,
            unit: None,
            last_deferred: None,
            bringup: request.sandbox == InitSandbox::RuntimeBringUp,
            draws: 0,
            logs: Vec::new(),
            remaining_log_bytes: request.log_bytes,
            seed_reads: 0,
        }
    }
}
struct Bindings {
    io: Option<Arc<wisp_host::bindings::PreparationIo>>,
    state: Arc<Mutex<Sandbox>>,
    memory_limit_bytes: Option<u64>,
}
impl wisp_engine::bindings::InitializationBindings for Bindings {
    fn permits_entropy(&self) -> bool {
        true
    }

    fn permits_io(&self) -> bool {
        self.io.is_some() && self.runtime_bringup()
    }
    fn native_io(
        &mut self,
        request: wisp_abi::native_io::Request,
    ) -> Result<wisp_abi::native_io::Response, wisp_abi::native_io::Error> {
        self.io
            .as_ref()
            .ok_or_else(|| {
                wisp_abi::native_io::Error::Denied("initialization I/O is disabled".into())
            })?
            .call(request)
    }
    fn slice_clock_ms(&self) -> u64 {
        if self.io.is_some() {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as u64)
        } else {
            0
        }
    }

    fn memory_limit_bytes(&self) -> Option<u64> {
        self.memory_limit_bytes
    }

    fn runtime_bringup(&self) -> bool {
        let state = self.state.lock();
        state.bringup && state.unit.is_none() && state.last_deferred.is_none()
    }
    fn call_sync(
        &mut self,
        call: ImportCall<'_>,
    ) -> Result<i64, wisp_engine::bindings::BindingError> {
        let mut state = self.state.lock();
        match call.import {
            "init_unit_begin" => {
                let unit = call.scalars[0] as u32;
                state.unit = (!matches!(unit, 0 | u32::MAX)).then_some(unit);
                state.last_deferred = None;
                Ok(0)
            }
            "init_unit_deferred" => {
                let unit = call.scalars[0] as u32;
                let deferred = state.deferred.contains(&unit);
                if deferred {
                    state.unit = None;
                    state.last_deferred = Some(unit);
                }
                Ok(i64::from(deferred))
            }
            "log_emit" => {
                let length = call.payload.len().min(state.remaining_log_bytes);
                if length > 0 {
                    state
                        .logs
                        .push(String::from_utf8_lossy(&call.payload[..length]).into_owned());
                    state.remaining_log_bytes -= length;
                }
                Ok(0)
            }
            _ => Err(wisp_engine::bindings::BindingError::Unsupported(
                call.import,
            )),
        }
    }
    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        output: &mut [u8],
    ) -> Result<i64, wisp_engine::bindings::BindingError> {
        let mut state = self.state.lock();
        match call.import {
            "init_hash_seed" => {
                if output.len() != 16 {
                    return Err(wisp_engine::bindings::BindingError::InvalidRequest(
                        "seed buffer must be exactly 16 bytes",
                    ));
                }
                state.seed_reads += 1;
                if !self.permits_entropy() {
                    return Err(wisp_engine::bindings::BindingError::Unsupported(
                        "entropy in shared runtime baseline",
                    ));
                }
                let seed = state.seed.get_or_insert_with(rand::random);
                output.copy_from_slice(seed);
                Ok(output.len() as i64)
            }
            "runtime_config_get" => {
                let length = output.len().min(state.runtime_config.len());
                output[..length].copy_from_slice(&state.runtime_config[..length]);
                Ok(state.runtime_config.len() as i64)
            }
            "init_config_get" => {
                let length = output.len().min(state.config.len());
                output[..length].copy_from_slice(&state.config[..length]);
                Ok(state.config.len() as i64)
            }
            "entropy_fill" if self.permits_entropy() => {
                rand::RngCore::fill_bytes(&mut rand::rng(), output);
                state.draws += 1;
                Ok(output.len() as i64)
            }
            _ => Err(wisp_engine::bindings::BindingError::Unsupported(
                call.import,
            )),
        }
    }
}

fn preparation_cpu() -> Result<u64, PrepareError> {
    wisp_engine::try_thread_cpu_nanos().ok_or(PrepareError::InvalidRequest(
        "preparation thread CPU clock is unavailable",
    ))
}

fn initialization_budget(
    request: &Initialization,
    spent_ns: u64,
    elapsed: Duration,
) -> Result<CpuBudget, PrepareError> {
    let remaining = Duration::from_millis(u64::from(request.wall_ms))
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(PrepareError::Limit("initialization wall time"))?;
    Ok(CpuBudget {
        cpu_ms: request.cpu_ms,
        wall_ms_cap: u32::try_from(
            remaining
                .as_nanos()
                .div_ceil(Duration::from_millis(1).as_nanos()),
        )
        .unwrap_or(u32::MAX),
        spent_ns,
    })
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn request() -> Initialization {
        Initialization {
            network: None,
            config: Vec::new(),
            runtime_config: b"{}".to_vec(),
            sandbox: InitSandbox::Sealed,
            cpu_ms: 50,
            wall_ms: 100,
            max_passes: 2,
            log_bytes: 0,
        }
    }

    #[test]
    fn wakes_and_replays_receive_only_the_remaining_initialization_budget() {
        let request = request();
        let initial = initialization_budget(&request, 0, Duration::ZERO).unwrap();
        let wake = initialization_budget(&request, 7_000_000, Duration::from_millis(75)).unwrap();
        assert_eq!(
            initial.wall_ms_cap, 100,
            "the initial slice must not extend its deadline"
        );
        assert_eq!(
            wake.wall_ms_cap, 25,
            "host waits and earlier passes must reduce the next wall budget"
        );
        assert_eq!(
            wake.spent_ns, 7_000_000,
            "CPU consumed before a wait must remain charged"
        );
        assert_eq!(
            wake.cpu_ms, initial.cpu_ms,
            "waking must retain the original total CPU budget"
        );
    }

    #[test]
    fn completion_at_or_after_the_initialization_deadline_is_refused() {
        for elapsed in [Duration::from_millis(100), Duration::from_millis(101)] {
            assert!(
                matches!(
                    initialization_budget(&request(), 0, elapsed),
                    Err(PrepareError::Limit("initialization wall time"))
                ),
                "a wake or final completion cannot extend the initialization deadline"
            );
        }
    }

    #[test]
    fn submillisecond_remainder_is_rounded_up_only_for_the_engine_slice() {
        let budget = initialization_budget(&request(), 0, Duration::from_micros(99_500)).unwrap();
        assert_eq!(
            budget.wall_ms_cap, 1,
            "the engine must receive a positive cap for a fractional remaining millisecond"
        );
    }
}
