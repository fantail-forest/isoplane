//! Runs the initialization contract through the same engine used by workers.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use isoplane_engine::{
    Compilation, Completion, ImportCall, InstanceOps, IsoplaneEngine, LoadedTemplate,
    OccupancyBudget,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{instrument, PrepareError};

/// Explicit deployment configuration and capability limits supplied by preparation.
#[derive(Clone, Serialize, Deserialize)]
pub struct Initialization {
    /// Explicit outbound authority; absent contexts cannot create host I/O.
    pub network: Option<crate::InitializationNetwork>,
    /// Deployment-bound JSON settings, including secrets; never include in diagnostics.
    pub runtime_config: Vec<u8>,
    /// Total initialization occupancy budget.
    pub occupancy_ms: u32,
    /// Total initialization wall backstop, including asynchronous waits.
    pub wall_ms: u32,
    /// Maximum retained guest log bytes.
    pub log_bytes: usize,
}

impl Initialization {
    /// An initialization with the deployment's settings, no outbound
    /// authority and the local default budgets. Set a field to change one.
    pub fn new(runtime_config: Vec<u8>) -> Self {
        Self {
            network: None,
            runtime_config,
            occupancy_ms: crate::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
            wall_ms: crate::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            log_bytes: crate::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        }
    }

    /// Identifies source, configuration and capability inputs for captured-state reuse.
    /// Fresh random draws and execution budgets are excluded from this input identity.
    pub fn digest(&self, input_hash: [u8; 32]) -> Result<[u8; 32], postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.initialization-input.v4\0");
        hash.update(&postcard::to_allocvec(&(
            input_hash,
            &self.runtime_config,
            &self.network,
        ))?);
        Ok(*hash.finalize().as_bytes())
    }
}

/// What initialization observed.
#[derive(Serialize, Deserialize)]
pub struct Initialized {
    /// Preparation occupancy across compilation, initialization and verification, excluding idle I/O waits.
    pub preparation_occupancy_ns: u64,
    /// Runtime capabilities initialization was answered.
    pub pal_calls: Vec<isoplane_engine::PalCallCount>,
    /// Bounded log output from initialization.
    pub logs: Vec<String>,
    /// Whether preparation compiled the submitted module rather than reusing the supplied cache's compilation.
    pub compiled: bool,
}

/// What initialization runs and how its result is captured. Every mode but
/// [`CaptureMode::Over`] loads `wasm`'s compilation from `cache`, compiling
/// it there on a miss, and keeps its working files in `cache`'s directory.
pub(crate) enum CaptureMode<'a> {
    /// The module's whole initialization, as a layer.
    #[cfg(all(target_os = "linux", feature = "rust-guests"))]
    Layered {
        wasm: &'a [u8],
        cache: &'a isoplane_engine::CompilationCache,
        identity: isoplane_engine::prepared::PreparedIdentity,
    },
    /// Only the runtime's own initializer, as a baseline that applications
    /// are prepared over.
    RuntimeBaseline {
        wasm: &'a [u8],
        cache: &'a isoplane_engine::CompilationCache,
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
            #[cfg(all(target_os = "linux", feature = "rust-guests"))]
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
        // Initialization is not a dispatch.
        outcome = instance.wake(
            0,
            completion.future,
            &completion.result,
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

pub(crate) struct InitializationOutput {
    pub initialized: Initialized,
    /// Initialized native code and explicit state.
    pub prepared: isoplane_engine::prepared::PreparedTemplate,
    /// The captured state loaded back, as it was verified against the
    /// initialized instance; absent for a module with no initializer.
    pub restored: Option<isoplane_engine::Template>,
}

pub(crate) fn initialize(
    engine: &IsoplaneEngine,
    request: &Initialization,
    application_files: &isoplane_engine::PlatformFiles,
    memory_limit_bytes: Option<u64>,
    capture: CaptureMode<'_>,
) -> Result<InitializationOutput, PrepareError> {
    if request.runtime_config.len() > isoplane_abi::imports::MAX_RUNTIME_CONFIGURATION_BYTES
        || serde_json::from_slice::<BTreeMap<String, String>>(&request.runtime_config).is_err()
    {
        return Err(PrepareError::InvalidRequest(
            "deployment settings must be a bounded JSON object of strings",
        ));
    }
    if request.occupancy_ms == 0 || request.wall_ms == 0 {
        return Err(PrepareError::InvalidRequest(
            "initialization limits must be positive",
        ));
    }
    let occupancy_start_ns = preparation_occupancy()?;
    let LoadedTemplate {
        template,
        compilation,
    } = load(engine, application_files, &capture)?;
    let compiled = compilation.compiled();
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
    let mut usage = InitializationUsage::default();
    let permits_entropy = !matches!(capture, CaptureMode::RuntimeBaseline { .. });
    let io = match (&request.network, &reactor) {
        (Some(network), Some(reactor)) => {
            let _entered = reactor.enter();
            Some(Arc::new(network.start(reactor.handle().clone())?))
        }
        _ => None,
    };
    let state = Arc::new(Mutex::new(Sandbox::new(request)));
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
                && instance.last_capability_violation().is_some()
            {
                return Err(PrepareError::Baseline("reached a forbidden capability"));
            }
            if let Some(import) = instance.last_capability_violation() {
                return Err(PrepareError::ForbiddenCapability {
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
        return match capture {
            #[cfg(all(target_os = "linux", feature = "rust-guests"))]
            CaptureMode::Layered { wasm, identity, .. } => Ok(InitializationOutput {
                prepared: engine
                    .prepare_template_with_files(wasm, identity, application_files.clone())
                    .map_err(|error| PrepareError::State(error.into()))?,
                restored: None,
                initialized: Initialized {
                    preparation_occupancy_ns: preparation_occupancy()?
                        .saturating_sub(occupancy_start_ns)
                        .saturating_sub(usage.unoccupied_ns),
                    pal_calls: instance
                        .initialization_pal_calls()
                        .map_err(PrepareError::State)?,
                    logs: Vec::new(),
                    // Without an initializer the original module is compiled for the template.
                    compiled: true,
                },
            }),
            CaptureMode::RuntimeBaseline { .. } | CaptureMode::Over { .. } => {
                Err(PrepareError::InvalidRequest(
                    "a runtime baseline needs a module with an initializer",
                ))
            }
        };
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
    // Reinstantiation verifies captured state without invoking the initializer again.
    let (restored_template, captured, prepared) = match capture {
        #[cfg(all(target_os = "linux", feature = "rust-guests"))]
        CaptureMode::Layered {
            identity, cache, ..
        } => {
            let prepared = engine
                .prepare_initialized_template(
                    &template,
                    &mut instance,
                    identity,
                    prepared_resources.clone(),
                )
                .map_err(|error| PrepareError::State(error.into()))?;
            let captured = instance
                .capture_mutable_state()
                .map_err(PrepareError::State)?;
            let template = engine
                .load_prepared_product_in(&prepared, cache.directory())
                .map_err(|error| PrepareError::State(error.into()))?;
            (template, captured, prepared)
        }
        CaptureMode::RuntimeBaseline {
            identity, cache, ..
        } => {
            let prepared = engine
                .prepare_runtime_baseline(
                    &template,
                    &mut instance,
                    identity,
                    prepared_resources.clone(),
                )
                .map_err(|error| PrepareError::State(error.into()))?;
            let captured = instance
                .capture_mutable_state()
                .map_err(PrepareError::State)?;
            let template = engine
                .load_prepared_product_in(&prepared, cache.directory())
                .map_err(|error| PrepareError::State(error.into()))?;
            (template, captured, prepared)
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
                )
                .map_err(|error| PrepareError::State(error.into()))?;
            let captured = instance
                .capture_mutable_state()
                .map_err(PrepareError::State)?;
            let template = engine
                .load_prepared_product_over_in(base, &prepared, storage)
                .map_err(|error| PrepareError::State(error.into()))?;
            (template, captured, prepared)
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
    Ok(InitializationOutput {
        prepared,
        restored: Some(restored_template),
        initialized: Initialized {
            preparation_occupancy_ns: preparation_occupancy()?
                .saturating_sub(occupancy_start_ns)
                .saturating_sub(usage.unoccupied_ns),
            pal_calls,
            logs,
            compiled,
        },
    })
}

/// The template initialization runs on, with the application's files bound,
/// and whether its compilation ran rather than reusing the cache's.
fn load(
    engine: &IsoplaneEngine,
    application_files: &isoplane_engine::PlatformFiles,
    capture: &CaptureMode<'_>,
) -> Result<LoadedTemplate, PrepareError> {
    let (wasm, cache) = match capture {
        #[cfg(all(target_os = "linux", feature = "rust-guests"))]
        CaptureMode::Layered { wasm, cache, .. } => (wasm, cache),
        CaptureMode::RuntimeBaseline { wasm, cache, .. } => (wasm, cache),
        CaptureMode::Over { base, .. } => {
            let template = base
                .clone()
                .with_application_files(application_files.clone())
                .map_err(PrepareError::State)?;
            return Ok(LoadedTemplate {
                template,
                compilation: Compilation::Reused,
            });
        }
    };
    let instrumented = instrument::instrument(wasm).map_err(PrepareError::State)?;
    let loaded = cache
        .load(engine, &instrumented)
        .map_err(PrepareError::State)?;
    let template = loaded
        .template
        .with_application_files(application_files.clone())
        .map_err(PrepareError::State)?;
    Ok(LoadedTemplate {
        template,
        compilation: loaded.compilation,
    })
}

/// `log_emit` severities run from trace at 0 to error at 4.
const WARNING: u64 = 3;

/// A line the guest logged, at the severity it gave.
struct GuestLog {
    level: u64,
    line: String,
}

/// The most a failure's detail keeps of what a guest logged.
const DETAIL_CHARACTERS: usize = 4096;

/// `detail` followed by `logs`, latest first: the result is bounded, so the
/// latest diagnosis comes before older context.
fn with_guest_logs<'a>(
    mut detail: String,
    logs: impl DoubleEndedIterator<Item = &'a GuestLog>,
) -> String {
    for log in logs.rev() {
        detail.push_str("\n  guest log: ");
        detail.push_str(log.line.trim_end());
    }
    detail.chars().take(DETAIL_CHARACTERS).collect()
}

struct Sandbox {
    runtime_config: Vec<u8>,
    logs: Vec<GuestLog>,
    remaining_log_bytes: usize,
}

impl Sandbox {
    fn new(request: &Initialization) -> Self {
        Self {
            runtime_config: request.runtime_config.clone(),
            logs: Vec::new(),
            remaining_log_bytes: request.log_bytes,
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
        self.io.is_some()
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

    fn call_sync(
        &mut self,
        call: ImportCall<'_>,
    ) -> Result<i64, isoplane_engine::bindings::BindingError> {
        let mut state = self.state.lock();
        match call.import {
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
        let state = self.state.lock();
        match call.import {
            "runtime_config_get" => {
                let length = output.len().min(state.runtime_config.len());
                output[..length].copy_from_slice(&state.runtime_config[..length]);
                Ok(state.runtime_config.len() as i64)
            }
            "entropy_fill" if self.permits_entropy => {
                isoplane_host::entropy::fill(output).map_err(|error| {
                    isoplane_engine::bindings::BindingError::Entropy(std::io::Error::other(error))
                })?;
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
            runtime_config: b"{}".to_vec(),
            occupancy_ms: 50,
            wall_ms: 100,
            log_bytes: 0,
        }
    }

    #[test]
    fn a_wake_receives_only_the_remaining_initialization_budget() {
        let request = request();
        let initial = initialization_budget(&request, 0, Duration::ZERO).unwrap();
        let wake = initialization_budget(&request, 7_000_000, Duration::from_millis(75)).unwrap();
        assert_eq!(
            initial.slice_wall_ms_cap, 100,
            "the initial slice must not extend its deadline"
        );
        assert_eq!(
            wake.slice_wall_ms_cap, 25,
            "host waits must reduce the next wall budget"
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
