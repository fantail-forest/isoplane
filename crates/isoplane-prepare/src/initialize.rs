//! Runs the initialization contract through the same engine used by workers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_abi::config::InitSandbox;
use isoplane_engine::{Completion, ImportCall, InstanceOps, IsoplaneEngine, OccupancyBudget};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

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
    /// Total initialization occupancy budget across replay passes.
    pub occupancy_ms: u32,
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
        hash.update(b"isoplane.initialization-input.v2\0");
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

/// What initialization observed and the replay state it leaves.
#[derive(Serialize, Deserialize)]
pub struct Initialized {
    /// Units deferred because they requested forbidden capabilities.
    pub deferred: Vec<u32>,
    /// Execution and reactor occupancy across all passes.
    pub occupancy_ns: u64,
    /// Preparation occupancy across compilation, replay and verification, excluding idle I/O waits.
    pub preparation_occupancy_ns: u64,
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
    /// Whether preparation compiled the submitted module rather than reusing the supplied cache's compilation.
    pub compiled: bool,
}

/// A portable initialization: the initialized module, ready for trusted
/// compilation, and what initialization observed.
pub struct InitializedModule {
    /// Portable initialized module; its init export is removed.
    pub wasm: Vec<u8>,
    /// What initialization observed and the replay state it leaves.
    pub initialized: Initialized,
}

/// Execute a validated application under the supplied worker engine in a preparation process.
/// Fails if initialization traps, exceeds a limit, or creates state the schema cannot preserve.
pub fn initialize(
    engine: &IsoplaneEngine,
    wasm: &[u8],
    request: &Initialization,
) -> Result<InitializedModule, PrepareError> {
    let output = initialize_with_cache(
        engine,
        request,
        None,
        &isoplane_engine::PlatformFiles::default(),
        None,
        CaptureMode::Portable { wasm },
    )?;
    match output.captured {
        Captured::Portable(wasm) => Ok(InitializedModule {
            wasm,
            initialized: output.initialized,
        }),
        Captured::Layered { .. } => Err(PrepareError::InvalidRequest(
            "portable initialization captured a layer",
        )),
    }
}

/// What initialization runs and how its result is captured. Every mode but
/// [`CaptureMode::Over`] compiles `wasm`, or loads its compilation from a cache.
pub(crate) enum CaptureMode<'a> {
    /// The module's whole initialization, as a portable module.
    Portable { wasm: &'a [u8] },
    /// The module's whole initialization, as a layer.
    #[cfg(target_os = "linux")]
    Layered {
        wasm: &'a [u8],
        identity: isoplane_engine::prepared::PreparedIdentity,
        portable: crate::PortableTemplate,
    },
    /// Only the runtime's own initializer, as a baseline that applications
    /// are prepared over.
    RuntimeBaseline {
        wasm: &'a [u8],
        identity: isoplane_engine::prepared::PreparedIdentity,
    },
    /// The application's initializer, run on an activation of `base`, as a
    /// layer over it.
    Over {
        base: isoplane_engine::Template,
        storage: &'a std::path::Path,
        identity: isoplane_engine::prepared::PreparedIdentity,
    },
}

impl CaptureMode<'_> {
    /// The initializer stages this capture runs, in order. Whole
    /// initialization runs the runtime stage first, as a baseline does, so a
    /// layer over a baseline ends in the state whole initialization leaves.
    fn stages(&self) -> (Stage, Option<Stage>) {
        match self {
            CaptureMode::Portable { .. } => (Stage::Runtime, Some(Stage::Application)),
            #[cfg(target_os = "linux")]
            CaptureMode::Layered { .. } => (Stage::Runtime, Some(Stage::Application)),
            CaptureMode::RuntimeBaseline { .. } => (Stage::Runtime, None),
            CaptureMode::Over { .. } => (Stage::Application, None),
        }
    }
}

/// One of a guest's initializer exports.
#[derive(Clone, Copy)]
enum Stage {
    /// `isoplane_initialize_runtime`, which completes at once in a guest
    /// without it.
    Runtime,
    /// `isoplane_initialize`.
    Application,
}

#[derive(Default)]
struct InitializationUsage {
    occupancy_ns: u64,
    unoccupied_ns: u64,
}

/// Runs a stage through its host wakes and final collection, excluding idle waits.
fn run_stage(
    instance: &mut isoplane_engine::Instance,
    stage: Stage,
    request: &Initialization,
    host: Option<(
        &isoplane_host::bindings::PreparationIo,
        &tokio::runtime::Runtime,
    )>,
    start: Instant,
    usage: &mut InitializationUsage,
) -> Result<isoplane_engine::DispatchOutcome, PrepareError> {
    let budget = initialization_budget(request, usage.occupancy_ns, start.elapsed())?;
    let mut outcome = match stage {
        Stage::Runtime => instance.run_initialize_runtime(budget),
        Stage::Application => instance.run_initialize(budget),
    };
    usage.occupancy_ns = usage.occupancy_ns.saturating_add(outcome.occupancy_ns);
    while outcome.completion == Completion::Parked {
        let Some((io, reactor)) = host else {
            return Err(PrepareError::InvalidRequest(
                "initializer is pending without an asynchronous host context",
            ));
        };
        let remaining = Duration::from_millis(u64::from(request.wall_ms))
            .checked_sub(start.elapsed())
            .ok_or(PrepareError::Limit("initialization wall time"))?;
        let reactor_start_ns = io.reactor_occupancy_ns();
        let wait_start = Instant::now();
        let completion = reactor
            .block_on(async { tokio::time::timeout(remaining, io.next_completion()).await })
            .map_err(|_| PrepareError::Limit("initialization wall time"))?
            .ok_or(PrepareError::InvalidRequest(
                "initializer is pending without a host wake source",
            ))?;
        let reactor_ns = io.reactor_occupancy_ns().saturating_sub(reactor_start_ns);
        usage.occupancy_ns = usage.occupancy_ns.saturating_add(reactor_ns);
        usage.unoccupied_ns = usage
            .unoccupied_ns
            .saturating_add((wait_start.elapsed().as_nanos() as u64).saturating_sub(reactor_ns));
        outcome = instance.wake(
            completion.0,
            &completion.1,
            initialization_budget(request, usage.occupancy_ns, start.elapsed())?,
        );
        usage.occupancy_ns = usage.occupancy_ns.saturating_add(outcome.occupancy_ns);
    }
    let ran_guest_code = match stage {
        Stage::Runtime => instance.has_initialize_runtime(),
        Stage::Application => true,
    };
    if outcome.completion != Completion::Done || !ran_guest_code {
        return Ok(outcome);
    }
    // Garbage left in a template is left to every instance: its first
    // collection, which a hibernation capture runs, rewrites template pages
    // into that instance's delta.
    let collected = instance.run_gc_hint(
        isoplane_host::hibernate_capture::GC_HINT_SNAPSHOT,
        initialization_budget(request, usage.occupancy_ns, start.elapsed())?,
    );
    usage.occupancy_ns = usage.occupancy_ns.saturating_add(collected.occupancy_ns);
    Ok(collected)
}

/// What a capture produced, in the representation its mode asked for.
pub(crate) enum Captured {
    /// The initialized portable module, which portable consumers compile.
    Portable(Vec<u8>),
    /// The initialized layer, and the portable module only when requested.
    Layered {
        prepared: Box<isoplane_engine::prepared::PreparedTemplate>,
        portable: Option<Vec<u8>>,
    },
}

pub(crate) struct InitializationOutput {
    pub initialized: Initialized,
    pub captured: Captured,
    /// The captured state loaded back, as it was verified against the
    /// initialized instance; absent for a module with no initializer.
    pub restored: Option<isoplane_engine::Template>,
}

pub(crate) fn initialize_with_cache(
    engine: &IsoplaneEngine,
    request: &Initialization,
    cache: Option<&isoplane_engine::CompilationCache>,
    application_files: &isoplane_engine::PlatformFiles,
    memory_limit_bytes: Option<u64>,
    capture: CaptureMode<'_>,
) -> Result<InitializationOutput, PrepareError> {
    let permits_entropy = !matches!(&capture, CaptureMode::RuntimeBaseline { .. });

    if request.runtime_config.len() > isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES
        || serde_json::from_slice::<BTreeMap<String, String>>(&request.runtime_config).is_err()
    {
        return Err(PrepareError::InvalidRequest(
            "deployment settings must be a bounded JSON object of strings",
        ));
    }
    if request.occupancy_ms == 0 || request.wall_ms == 0 || request.max_passes == 0 {
        return Err(PrepareError::InvalidRequest(
            "initialization limits must be positive",
        ));
    }
    let occupancy_start_ns = preparation_occupancy()?;
    let mut deferred_reasons = Vec::new();
    let mut capability_refusals = 0;
    let (template, instrumented, compiled) = load(engine, cache, application_files, &capture)?;
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
    let mut usage = InitializationUsage::default();
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
                    permits_entropy,
                }),
            )
            .map_err(PrepareError::State)?;
        if matches!(capture, CaptureMode::RuntimeBaseline { .. })
            && (!template.has_initialize() || !instance.has_initialize_runtime())
        {
            return Err(PrepareError::InvalidRequest(
                "a runtime baseline requires both initialization exports",
            ));
        }
        if template.has_initialize() {
            let host = io.as_deref().zip(reactor.as_ref());
            let (first, then) = capture.stages();
            let mut outcome = run_stage(
                &mut instance,
                first,
                request,
                host,
                initialization_start,
                &mut usage,
            )?;
            if let Some(then) = then.filter(|_| outcome.completion == Completion::Done) {
                outcome = run_stage(
                    &mut instance,
                    then,
                    request,
                    host,
                    initialization_start,
                    &mut usage,
                )?;
            }
            initialization_budget(request, usage.occupancy_ns, initialization_start.elapsed())?;
            if outcome.completion != Completion::Done {
                if matches!(capture, CaptureMode::RuntimeBaseline { .. })
                    && (instance.last_capability_violation().is_some()
                        || state.lock().seed_reads > 0)
                {
                    return Err(PrepareError::Baseline("reached a forbidden capability"));
                }
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
                let detail = outcome
                    .trap
                    .map(|trap| trap.message)
                    .unwrap_or_else(|| "initializer did not return successfully".into());
                return Err(PrepareError::Initialization {
                    completion: outcome.completion,
                    detail: with_guest_logs(detail, state.lock().logs.iter()),
                });
            }
        } else {
            let captured = match capture {
                CaptureMode::Portable { wasm } => Captured::Portable(wasm.to_vec()),
                #[cfg(target_os = "linux")]
                CaptureMode::Layered {
                    wasm,
                    identity,
                    portable,
                } => Captured::Layered {
                    prepared: Box::new(
                        engine
                            .prepare_template_with_files(wasm, identity, application_files.clone())
                            .map_err(|error| PrepareError::State(error.into()))?,
                    ),
                    portable: (portable == crate::PortableTemplate::Returned)
                        .then(|| wasm.to_vec()),
                },
                CaptureMode::RuntimeBaseline { .. } | CaptureMode::Over { .. } => {
                    return Err(PrepareError::InvalidRequest(
                        "a runtime baseline needs a module with an initializer",
                    ))
                }
            };
            return Ok(InitializationOutput {
                captured,
                restored: None,
                initialized: Initialized {
                    deferred: Vec::new(),
                    occupancy_ns: usage.occupancy_ns,
                    preparation_occupancy_ns: preparation_occupancy()?
                        .saturating_sub(occupancy_start_ns)
                        .saturating_sub(usage.unoccupied_ns),
                    deferred_reasons,
                    capability_refusals,
                    pal_calls: instance
                        .initialization_pal_calls()
                        .map_err(PrepareError::State)?,
                    seed_reads: state.lock().seed_reads,
                    passes: 0,
                    logs: Vec::new(),
                    // Without an initializer the original module is compiled for the template.
                    compiled: true,
                },
            });
        }
        let prepared_resources = io
            .as_ref()
            .map(|io| io.finish())
            .transpose()
            .map_err(|error| {
                // The initializer returned successfully, so of what it logged
                // only its warnings bear on why its state cannot be captured.
                let state = state.lock();
                let warnings = state.logs.iter().filter(|log| log.level >= WARNING);
                PrepareError::State(anyhow::anyhow!(with_guest_logs(
                    format!("initialization retained native state: {error}"),
                    warnings,
                )))
            })?
            .unwrap_or_default();
        let prepared_resources = isoplane_abi::native_io::PreparedResources {
            timers_between_requests_ms: state.lock().timers_between_requests_ms,
            ..prepared_resources
        };
        let deferred: Vec<_> = deferred.into_iter().collect();
        if matches!(capture, CaptureMode::RuntimeBaseline { .. }) {
            let state = state.lock();
            if state.seed_reads > 0 || state.draws > 0 {
                return Err(PrepareError::Baseline("read initialization entropy"));
            }
        }
        // Reinstantiation verifies captured state without invoking the initializer again.
        let (restored_template, captured, representation) = match capture {
            CaptureMode::Portable { .. } => {
                let instrumented = instrumented.as_ref().ok_or(PrepareError::InvalidRequest(
                    "a portable capture needs the instrumented module",
                ))?;
                let captured = instance
                    .capture_mutable_state()
                    .map_err(PrepareError::State)?;
                let module = portable_module(
                    instrumented,
                    instance.memory(),
                    &captured.globals,
                    &deferred,
                    &prepared_resources,
                )?;
                let template = engine
                    .load_template(&module)
                    .and_then(|template| template.with_application_files(application_files.clone()))
                    .map_err(PrepareError::State)?;
                (template, captured, Captured::Portable(module))
            }
            #[cfg(target_os = "linux")]
            CaptureMode::Layered {
                identity, portable, ..
            } => {
                let prepared = engine
                    .prepare_initialized_template(
                        &template,
                        &mut instance,
                        identity,
                        prepared_resources.clone(),
                        deferred.clone(),
                    )
                    .map_err(|error| PrepareError::State(error.into()))?;
                let captured = instance
                    .capture_mutable_state()
                    .map_err(PrepareError::State)?;
                let portable = match (portable, &instrumented) {
                    (crate::PortableTemplate::Returned, Some(instrumented)) => {
                        Some(portable_module(
                            instrumented,
                            instance.memory(),
                            &captured.globals,
                            &deferred,
                            &prepared_resources,
                        )?)
                    }
                    (crate::PortableTemplate::Returned, None) => {
                        return Err(PrepareError::InvalidRequest(
                            "a portable capture needs the instrumented module",
                        ))
                    }
                    (crate::PortableTemplate::Omitted, _) => None,
                };
                let template = unsafe {
                    // SAFETY: this engine captured `prepared` above; its native bytes came from that compiler.
                    engine.load_prepared_product_in(
                        &prepared,
                        cache.map_or(std::env::temp_dir().as_path(), |cache| cache.directory()),
                    )
                }
                .map_err(|error| PrepareError::State(error.into()))?;
                (
                    template,
                    captured,
                    Captured::Layered {
                        prepared: Box::new(prepared),
                        portable,
                    },
                )
            }
            CaptureMode::RuntimeBaseline { identity, .. } => {
                let prepared = engine
                    .prepare_runtime_baseline(
                        &template,
                        &mut instance,
                        identity,
                        prepared_resources.clone(),
                        deferred.clone(),
                    )
                    .map_err(|error| PrepareError::State(error.into()))?;
                let captured = instance
                    .capture_mutable_state()
                    .map_err(PrepareError::State)?;
                let template = unsafe {
                    // SAFETY: this engine captured `prepared` above; its native bytes came from that compiler.
                    engine.load_prepared_product_in(
                        &prepared,
                        cache.map_or(std::env::temp_dir().as_path(), |cache| cache.directory()),
                    )
                }
                .map_err(|error| PrepareError::State(error.into()))?;
                (
                    template,
                    captured,
                    Captured::Layered {
                        prepared: Box::new(prepared),
                        portable: None,
                    },
                )
            }
            CaptureMode::Over {
                ref base,
                identity,
                storage,
            } => {
                let prepared = engine
                    .prepare_initialized_template_over(
                        &template,
                        &mut instance,
                        identity,
                        prepared_resources.clone(),
                        deferred.clone(),
                    )
                    .map_err(|error| PrepareError::State(error.into()))?;
                let captured = instance
                    .capture_mutable_state()
                    .map_err(PrepareError::State)?;
                let template = unsafe {
                    // SAFETY: this engine captured `prepared` above; its native bytes came from that compiler.
                    engine.load_prepared_product_over_in(base, &prepared, storage)
                }
                .map_err(|error| PrepareError::State(error.into()))?;
                (
                    template,
                    captured,
                    Captured::Layered {
                        prepared: Box::new(prepared),
                        portable: None,
                    },
                )
            }
        };
        let mut restored = engine
            .activate_initialization(
                &restored_template,
                Box::new(Bindings {
                    state: state.clone(),
                    io: io.clone(),
                    memory_limit_bytes,
                    permits_entropy,
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
        let logs = state.logs.iter().map(|log| log.line.clone()).collect();
        return Ok(InitializationOutput {
            captured: representation,
            restored: Some(restored_template),
            initialized: Initialized {
                deferred,
                occupancy_ns: usage.occupancy_ns,
                preparation_occupancy_ns: preparation_occupancy()?
                    .saturating_sub(occupancy_start_ns)
                    .saturating_sub(usage.unoccupied_ns),
                deferred_reasons,
                capability_refusals,
                pal_calls,
                seed_reads: state.seed_reads,
                passes: pass,
                logs,
                compiled,
            },
        });
    }
    Err(PrepareError::NoConvergence(request.max_passes))
}

/// The template initialization runs on, with the application's files bound;
/// the instrumented module when the template was compiled from it; and
/// whether that compilation ran rather than reusing `cache`'s.
fn load(
    engine: &IsoplaneEngine,
    cache: Option<&isoplane_engine::CompilationCache>,
    application_files: &isoplane_engine::PlatformFiles,
    capture: &CaptureMode<'_>,
) -> Result<(isoplane_engine::Template, Option<bake::Instrumented>, bool), PrepareError> {
    let wasm = match capture {
        CaptureMode::Portable { wasm } => wasm,
        #[cfg(target_os = "linux")]
        CaptureMode::Layered { wasm, .. } => wasm,
        CaptureMode::RuntimeBaseline { wasm, .. } => wasm,
        CaptureMode::Over { base, .. } => {
            let template = base
                .clone()
                .with_application_files(application_files.clone())
                .map_err(PrepareError::State)?;
            return Ok((template, None, false));
        }
    };
    let instrumented = bake::instrument(wasm).map_err(PrepareError::State)?;
    let (template, compiled) = match cache {
        Some(cache) => cache.load(engine, &instrumented.wasm),
        None => match capture {
            CaptureMode::Portable { .. } => engine.load_template(&instrumented.wasm),
            #[cfg(target_os = "linux")]
            CaptureMode::Layered { .. } => engine.load_layer_source(&instrumented.wasm),
            CaptureMode::RuntimeBaseline { .. } | CaptureMode::Over { .. } => {
                engine.load_layer_source(&instrumented.wasm)
            }
        }
        .map(|template| (template, true)),
    }
    .map_err(PrepareError::State)?;
    let template = template
        .with_application_files(application_files.clone())
        .map_err(PrepareError::State)?;
    Ok((template, Some(instrumented), compiled))
}

/// The instrumented module with the initialized memory, globals and deferred
/// units baked in, and the resources the host holds for it.
fn portable_module(
    instrumented: &bake::Instrumented,
    memory: &[u8],
    globals: &[isoplane_engine::snapshot::GlobalValue],
    deferred: &[u32],
    resources: &isoplane_abi::native_io::PreparedResources,
) -> Result<Vec<u8>, PrepareError> {
    let globals = captured_globals(instrumented, globals)?;
    let mut module =
        bake::bake(&instrumented.wasm, memory, &globals, deferred).map_err(PrepareError::State)?;
    let resources = postcard::to_allocvec(resources).map_err(PrepareError::Encoding)?;
    wasm_encoder::Section::append_to(
        &wasm_encoder::CustomSection {
            name: isoplane_abi::native_io::PREPARED_RESOURCES_SECTION.into(),
            data: resources.into(),
        },
        &mut module,
    );
    Ok(module)
}

fn captured_globals(
    instrumented: &bake::Instrumented,
    values: &[isoplane_engine::snapshot::GlobalValue],
) -> Result<BTreeMap<u32, isoplane_engine::snapshot::GlobalValue>, PrepareError> {
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

/// `log_emit` severities run from trace at 0 to error at 4.
const WARNING: u64 = 3;

/// A line the guest logged, at the severity it gave.
struct GuestLog {
    level: u64,
    line: String,
}

/// `detail` followed by `logs`, latest first: worker reports are bounded, so
/// the latest diagnosis comes before older context.
fn with_guest_logs<'a>(
    mut detail: String,
    logs: impl DoubleEndedIterator<Item = &'a GuestLog>,
) -> String {
    for log in logs.rev() {
        detail.push_str("\n  guest log: ");
        detail.push_str(log.line.trim_end());
    }
    detail
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
    logs: Vec<GuestLog>,
    remaining_log_bytes: usize,
    seed_reads: u32,
    timers_between_requests_ms: Option<u64>,
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
            timers_between_requests_ms: None,
        }
    }
}
struct Bindings {
    io: Option<Arc<isoplane_host::bindings::PreparationIo>>,
    state: Arc<Mutex<Sandbox>>,
    memory_limit_bytes: Option<u64>,
    permits_entropy: bool,
}
impl isoplane_engine::bindings::InitializationBindings for Bindings {
    fn permits_entropy(&self) -> bool {
        self.permits_entropy
    }

    fn permits_io(&self) -> bool {
        self.io.is_some() && self.runtime_bringup()
    }
    fn native_io(
        &mut self,
        request: isoplane_abi::native_io::Request,
    ) -> Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error> {
        self.io
            .as_ref()
            .ok_or_else(|| {
                isoplane_abi::native_io::Error::Denied("initialization I/O is disabled".into())
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

    fn begin_slice(&mut self) {
        if let Some(io) = &self.io {
            io.begin_slice();
        }
    }

    fn runtime_bringup(&self) -> bool {
        let state = self.state.lock();
        state.bringup && state.unit.is_none() && state.last_deferred.is_none()
    }
    fn call_sync(
        &mut self,
        call: ImportCall<'_>,
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
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
            "timers_between_requests" => match call.scalars[0] {
                minimum_interval_ms
                    if minimum_interval_ms < isoplane_abi::imports::MIN_TIMERS_BETWEEN_REQUESTS_MS =>
                {
                    Err(isoplane_engine::bindings::BindingError::InvalidRequest(
                        "the least time between timer wakes is below the platform's minimum of 100 ms",
                    ))
                }
                minimum_interval_ms => {
                    state.timers_between_requests_ms = Some(minimum_interval_ms);
                    Ok(0)
                }
            },
            "log_emit" => {
                let length = call.payload.len().min(state.remaining_log_bytes);
                if length > 0 {
                    state.logs.push(GuestLog {
                        level: call.scalars[0],
                        line: String::from_utf8_lossy(&call.payload[..length]).into_owned(),
                    });
                    state.remaining_log_bytes -= length;
                }
                Ok(0)
            }
            _ => Err(isoplane_engine::bindings::BindingError::Unsupported(
                call.import,
            )),
        }
    }
    fn call_fill(
        &mut self,
        call: ImportCall<'_>,
        output: &mut [u8],
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        let mut state = self.state.lock();
        match call.import {
            "init_hash_seed" => {
                if output.len() != 16 {
                    return Err(isoplane_engine::bindings::BindingError::InvalidRequest(
                        "seed buffer must be exactly 16 bytes",
                    ));
                }
                state.seed_reads += 1;
                if !self.permits_entropy() {
                    return Err(isoplane_engine::bindings::BindingError::Unsupported(
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
            _ => Err(isoplane_engine::bindings::BindingError::Unsupported(
                call.import,
            )),
        }
    }
}

fn preparation_occupancy() -> Result<u64, PrepareError> {
    isoplane_engine::try_occupancy_nanos().ok_or(PrepareError::InvalidRequest(
        "preparation worker occupancy clock is unavailable",
    ))
}

fn initialization_budget(
    request: &Initialization,
    spent_ns: u64,
    elapsed: Duration,
) -> Result<OccupancyBudget, PrepareError> {
    let remaining = Duration::from_millis(u64::from(request.wall_ms))
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or(PrepareError::Limit("initialization wall time"))?;
    Ok(OccupancyBudget {
        occupancy_ms: request.occupancy_ms,
        slice_wall_ms_cap: u32::try_from(
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
            occupancy_ms: 50,
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
            initial.slice_wall_ms_cap, 100,
            "the initial slice must not extend its deadline"
        );
        assert_eq!(
            wake.slice_wall_ms_cap, 25,
            "host waits and earlier passes must reduce the next wall budget"
        );
        assert_eq!(
            wake.spent_ns, 7_000_000,
            "Occupancy consumed before a wait must remain charged"
        );
        assert_eq!(
            wake.occupancy_ms, initial.occupancy_ms,
            "waking must retain the original total occupancy budget"
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
            budget.slice_wall_ms_cap, 1,
            "the engine must receive a positive cap for a fractional remaining millisecond"
        );
    }
}
