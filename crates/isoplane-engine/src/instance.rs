//! A live activation: the template mapped copy-on-write, its mutable globals
//! and tables, and whether it was discarded.

use std::sync::Arc;
use std::time::Instant;

use wasmtime::{Global, Memory, Store, Table, TypedFunc};

use isoplane_abi::dispatch::DispatchRecord;
use isoplane_abi::result::ResultRecord;
use isoplane_abi::status::PollStatus;

use crate::budget::{ExecutionLimit, GraceEnd, OccupancyBudget, OccupancyMeasurement, TrapKind};
use crate::call::{call_entry, classify, trap_code_of_kind, trap_kind_of_code};
use crate::meter::interval_occupancy_ns;
use crate::state::{Meter, Reentrancy, StoreState};
use crate::template::TemplateInner;
use crate::{DispatchOutcome, EngineInner, InstanceImage, Result, TrapInfo};

/// The wasmtime side of an activation.
pub(crate) struct Live {
    /// Watches memory mapped from the template's storage; declared first so
    /// the range is released before the store frees it.
    #[cfg(target_os = "linux")]
    pub(crate) storage: Option<crate::storage_fault::Watch>,
    pub(crate) store: Store<StoreState>,
    pub(crate) memory: Memory,
    pub(crate) f_alloc: TypedFunc<(i32, i32), i32>,
    pub(crate) f_handle: TypedFunc<(i32, i32), i64>,
    pub(crate) f_wake: TypedFunc<(i64, i32, i32), i64>,
    pub(crate) f_gc_hint: Option<TypedFunc<i32, ()>>,
    pub(crate) f_initialize: Option<TypedFunc<(), i64>>,
    pub(crate) f_initialize_runtime: Option<TypedFunc<(), i64>>,
    pub(crate) globals: Vec<Global>,
    pub(crate) tables: Vec<Table>,
    pub(crate) table_baseline: crate::capture::TableBaseline,
}

pub struct Instance {
    pub(crate) live: Live,
    // Unmap the live store before the template releases its storage leases.
    pub(crate) template: Arc<TemplateInner>,
    pub(crate) engine: Arc<EngineInner>,
    /// Set when a call ended in a way [`Completion::instance_survives`] says leaves
    /// the instance inconsistent. wasmtime does not poison a store after a
    /// trap, so every entry point checks this and refuses.
    pub(crate) discarded: Option<&'static str>,
    pub(crate) last: Option<Completion>,
    pub(crate) last_violation: Option<&'static str>,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("template", &self.template.memory_pages)
            .field("discarded", &self.discarded)
            .field("last", &self.last)
            .finish()
    }
}

pub(crate) enum Entry<'a> {
    Handle(&'a DispatchRecord),
    /// The dispatch the slice runs, the completed future and its result.
    Wake(u64, u64, &'a ResultRecord),
    GcHint(u8),
    Initialize,
    InitializeRuntime,
}

impl Entry<'_> {
    fn export_name(&self) -> &'static str {
        match self {
            Entry::Handle(_) => "isoplane_handle",
            Entry::Wake(..) => "isoplane_wake",
            Entry::GcHint(_) => "isoplane_gc_hint",
            Entry::Initialize => "isoplane_initialize",
            Entry::InitializeRuntime => "isoplane_initialize_runtime",
        }
    }
}

struct HotSlice<'a>(&'a crate::watchdog::Shared);

impl<'a> HotSlice<'a> {
    fn new(watchdog: &'a crate::watchdog::Shared) -> Self {
        watchdog.enter_hot();
        Self(watchdog)
    }
}

impl Drop for HotSlice<'_> {
    fn drop(&mut self) {
        self.0.exit_hot();
    }
}

impl Instance {
    /// Runtime capabilities answered during initialization, counted at the engine boundary.
    pub fn initialization_pal_calls(&self) -> Result<Vec<(String, u32)>> {
        Ok(self
            .live
            .store
            .data()
            .init_pal_calls
            .iter()
            .map(|(name, count)| (name.to_string(), *count))
            .collect())
    }

    /// The import that tripped capability policy on the last call, if any.
    /// Pre-initialisation identifies an impure static initialiser by which
    /// forbidden import it touched.
    pub fn last_capability_violation(&self) -> Option<&'static str> {
        self.last_violation
    }

    /// Run `isoplane_initialize`, for pre-initialisation with bindings whose
    /// `is_init_sandbox()` is true. Ordinary activation never calls this; the
    /// template already ran it.
    pub fn run_initialize(&mut self, budget: OccupancyBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Initialize)
    }

    /// Run `isoplane_initialize_runtime`, which starts only the guest's runtime,
    /// before any application code or data loads. A guest without the export
    /// has no separate runtime stage and completes at once.
    pub fn run_initialize_runtime(&mut self, budget: OccupancyBudget) -> DispatchOutcome {
        self.slice(budget, Entry::InitializeRuntime)
    }

    /// Whether the guest exports `isoplane_initialize_runtime`, so that
    /// [`Instance::run_initialize_runtime`] runs guest code.
    pub fn has_initialize_runtime(&self) -> bool {
        self.live.f_initialize_runtime.is_some()
    }

    /// Run `isoplane_gc_hint` under `budget`, which preparation charges to
    /// initialization. A guest without the export completes at once.
    pub fn run_gc_hint(&mut self, pressure: u8, budget: OccupancyBudget) -> DispatchOutcome {
        self.slice(budget, Entry::GcHint(pressure))
    }

    fn refuse(&self, kind: TrapKind, why: String) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Trapped {
                code: trap_code_of_kind(kind),
            },
            completion: Completion::Trapped,
            occupancy_ns: 0,
            occupancy_measurement: OccupancyMeasurement::NotExecuted,
            execution_limit: None,
            wall_ns: 0,
            trap: Some(TrapInfo { kind, message: why }),
            grace_end: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    /// One slice: meter start, epoch deadline armed, synchronous call into
    /// the guest, meter stop.
    fn slice(&mut self, budget: OccupancyBudget, entry: Entry<'_>) -> DispatchOutcome {
        if let Some(why) = self.discarded {
            return self.refuse(
                TrapKind::Discarded,
                format!(
                    "`{}` called on a discarded instance, discarded because {why}",
                    entry.export_name()
                ),
            );
        }
        let engine = self.engine.clone();
        let live = &mut self.live;

        // A reentrant call would corrupt the meter and the frozen clock.
        let depth = live.store.data().wasm_depth;
        if depth != 0 {
            let e: anyhow::Error = Reentrancy { depth }.into();
            return self.refuse(TrapKind::Other, format!("{e}"));
        }

        let clock_ms = live
            .store
            .data_mut()
            .bindings
            .as_mut()
            .map(|b| {
                // SECURITY: fixes what the guest observes for the whole slice;
                // see `HostBindings::begin_slice`.
                b.begin_slice();
                b.slice_clock_ms()
            })
            .unwrap_or(0);

        let meter = Meter {
            occupancy0_ns: 0,
            wall0: None,
            occupancy_budget_ns: budget.budget_ns(),
            grace_ns: budget.grace_ns(),
            spent_ns: budget.spent_ns,
            slice_wall_cap_ns: (budget.slice_wall_ms_cap as u64).saturating_mul(1_000_000),
            check_ticks: 1,
        };

        // A gc hint runs on a parked instance immediately before capture and
        // must not clear the park state.
        let is_gc_hint = matches!(entry, Entry::GcHint(_));

        let occupancy_start_ns = match crate::meter::occupancy_nanos() {
            Ok(occupancy_start_ns) => occupancy_start_ns,
            Err(error) => {
                return self.refuse(TrapKind::OccupancyClockUnavailable, error.to_string())
            }
        };

        // The watchdog only ticks while something is hot, so this precedes
        // arming the deadline.
        let hot = HotSlice::new(engine.watchdog.shared());

        let wall0 = Instant::now();
        live.store.data_mut().begin_slice(
            clock_ms,
            Meter {
                occupancy0_ns: occupancy_start_ns,
                wall0: Some(wall0),
                ..meter
            },
        );
        live.store.set_epoch_deadline(meter.check_ticks);

        let raw = call_entry(live, entry);

        // Classification and teardown are outside the slice's occupancy meter.
        let occupancy_end_ns = interval_occupancy_ns(occupancy_start_ns);
        let (occupancy_ns, occupancy_measurement) = match occupancy_end_ns {
            Some(occupancy_ns) => (occupancy_ns, OccupancyMeasurement::Complete),
            None => (0, OccupancyMeasurement::Incomplete),
        };
        let wall_ns = wall0.elapsed().as_nanos() as u64;

        live.store.set_epoch_deadline(u64::MAX / 2);
        drop(hot);

        let (status, trap) = match raw {
            Ok(v) => {
                let status = PollStatus::decode(v);
                let trap = match status {
                    PollStatus::Trapped { code } => Some(TrapInfo {
                        kind: trap_kind_of_code(code),
                        message: format!("guest returned trapped({code:?})"),
                    }),
                    _ => None,
                };
                (status, trap)
            }
            Err(e) => {
                let info = classify(live, &e);
                (
                    PollStatus::Trapped {
                        code: trap_code_of_kind(info.kind),
                    },
                    Some(info),
                )
            }
        };

        // Host code that read failed storage beneath this instance saw zeros in
        // its place, so whatever the slice returned was computed from memory the
        // instance does not hold.
        #[cfg(target_os = "linux")]
        let (status, trap) = if live
            .storage
            .as_ref()
            .is_some_and(crate::storage_fault::Watch::faulted)
        {
            (
                PollStatus::Trapped {
                    code: trap_code_of_kind(TrapKind::StorageFault),
                },
                Some(TrapInfo {
                    kind: TrapKind::StorageFault,
                    message: "host code read this instance's memory after the template storage \
                              beneath it failed"
                        .to_string(),
                }),
            )
        } else {
            (status, trap)
        };
        if trap
            .as_ref()
            .is_some_and(|trap| trap.kind == TrapKind::StorageFault)
        {
            self.template.fail_storage();
        }

        let violation = live.store.data().violation;
        let grace_at_ns = live.store.data().grace_at_ns;
        let grace_deferrals = live.store.data().grace_deferrals;

        // A `BudgetExceeded` trap comes only from the hard deadline. A slice
        // that entered grace and returned `done` is an exit; one that
        // returned `pending` parked at an await and the wake that follows
        // carries `spent_ns`, so it is not classified yet.
        let hard_kill = matches!(
            trap.as_ref().map(|t| t.kind),
            Some(TrapKind::BudgetExceeded)
        );
        let dispatch_occupancy_ns = budget.spent_ns.saturating_add(occupancy_ns);
        // Crossing B counts as grace whether or not the epoch callback fired
        // first: a guest can return between ticks, and classifying only on
        // `grace_at_ns` would report it as an ordinary `done` with unmeasured
        // overage.
        let in_grace = grace_at_ns.is_some() || dispatch_occupancy_ns >= budget.budget_ns();
        if let Some(bindings) = live.store.data_mut().bindings.as_mut() {
            // A slice can cross B between epoch ticks, so grace is entered here
            // when no tick entered it. A kill has nothing left to cancel.
            if in_grace && grace_at_ns.is_none() && !hard_kill {
                bindings.enter_grace();
            }
            bindings.end_slice();
        }
        let completion = if hard_kill {
            Completion::BudgetKill
        } else {
            match Completion::from_status(status) {
                Completion::Done | Completion::Cancelled | Completion::Failed if in_grace => {
                    Completion::BudgetExit
                }
                other => other,
            }
        };
        let grace_end = match (hard_kill, completion) {
            (true, _) => Some(GraceEnd::Killed),
            (false, Completion::BudgetExit) => Some(GraceEnd::Exited),
            _ => None,
        };
        let overage_ns = if in_grace || hard_kill {
            dispatch_occupancy_ns.saturating_sub(budget.budget_ns())
        } else {
            0
        };

        if !(is_gc_hint && completion.instance_survives()) {
            self.last = Some(completion);
        }
        self.last_violation = violation;
        if !completion.instance_survives() {
            self.discarded = Some(match completion {
                Completion::BudgetKill => {
                    "it was still executing at the B+G hard deadline and was terminated without \
                     unwinding"
                }
                _ => "it trapped; traps are never retried on the same memory",
            });
        }

        DispatchOutcome {
            status,
            completion,
            occupancy_ns,
            occupancy_measurement,
            execution_limit: match live.store.data().kill {
                Some(crate::state::Kill::OccupancyHard) => Some(ExecutionLimit::Occupancy),
                Some(crate::state::Kill::Wall) => Some(ExecutionLimit::Wall),
                None if completion == Completion::BudgetExit => Some(ExecutionLimit::Occupancy),
                None => None,
            },
            wall_ns,
            trap,
            grace_end,
            overage_ns,
            grace_deferrals,
        }
    }
}

/// What the host does to a live instance. A trait so `isoplane-host` can be
/// tested against a fake engine.
pub trait InstanceOps {
    /// Discard resident linear-memory pages after a checkpoint has been saved.
    /// Returns retained mappings when supported, otherwise releases the instance.
    fn evict(self: Box<Self>) -> Option<Box<dyn crate::DormantInstance>> {
        None
    }
    /// Run a dispatch to completion, park, or kill.
    fn dispatch(&mut self, rec: &DispatchRecord, budget: OccupancyBudget) -> DispatchOutcome;
    /// Deliver a completed host future to the dispatch that owns it, in a
    /// slice of that dispatch.
    fn wake(
        &mut self,
        dispatch: u64,
        future: u64,
        res: &ResultRecord,
        budget: OccupancyBudget,
    ) -> DispatchOutcome;
    /// Called before hibernation capture so the guest GC can compact and zero
    /// the freed tail; zeroed pages compress out of the delta.
    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome;
    /// True only when the last slice returned `pending` and wasmtime reports
    /// no wasm frame on the native stack.
    fn is_parked(&self) -> bool;
    /// True when the instance may be captured: the stack is empty, the last
    /// slice ended in a state the instance survives, and it is not discarded.
    /// Weaker than [`InstanceOps::is_parked`] on purpose: an instance that
    /// returned `done` and went idle is the case hibernation is about.
    fn is_capture_eligible(&self) -> bool;
    /// Linear memory.
    fn memory(&mut self) -> &mut [u8];
    /// Whether host code read storage beneath this instance after it failed,
    /// so its memory, or a comparison against its template, is no longer what
    /// execution produced. Hibernation asks after reading memory.
    fn storage_failed(&self) -> bool;
    fn memory_len(&self) -> usize;
    /// Byte ranges of linear memory that may differ from the template, in
    /// ascending order, or `None` when the instance cannot tell and every page
    /// has to be compared with the template.
    fn changed_ranges(&mut self) -> Option<Vec<std::ops::Range<u64>>> {
        None
    }
    /// Globals and tables. Linear memory is diffed by `isoplane-hibernate`.
    fn capture_mutable_state(&mut self) -> Result<InstanceImage>;
    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()>;
}

/// How a slice ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Completion {
    Done,
    /// The dispatch cooperatively cancelled, leaving a reusable instance.
    Cancelled,
    /// An application exception failed the dispatch, leaving a reusable instance.
    Failed,
    /// Returned `pending`: empty stack, futures registered.
    Parked,
    /// Guest fault. The instance is discarded.
    Trapped,
    /// Still executing at B+G and terminated without unwinding. The instance
    /// is discarded: its memory is a state no execution produced.
    BudgetKill,
    /// Entered grace and returned inside it. The dispatch still fails, but
    /// nothing was killed: the instance is consistent and stays.
    BudgetExit,
}

impl Completion {
    pub fn from_status(s: PollStatus) -> Self {
        match s {
            PollStatus::Done { .. } => Completion::Done,
            PollStatus::Cancelled => Completion::Cancelled,
            PollStatus::Failed => Completion::Failed,
            PollStatus::Pending => Completion::Parked,
            PollStatus::Trapped { .. } => Completion::Trapped,
        }
    }
    /// Whether the instance may be reused.
    pub fn instance_survives(self) -> bool {
        matches!(
            self,
            Completion::Done
                | Completion::Cancelled
                | Completion::Failed
                | Completion::Parked
                | Completion::BudgetExit
        )
    }
}

impl InstanceOps for Instance {
    fn evict(self: Box<Self>) -> Option<Box<dyn crate::DormantInstance>> {
        crate::mapped::evict(*self)
    }
    fn dispatch(&mut self, rec: &DispatchRecord, budget: OccupancyBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Handle(rec))
    }

    fn wake(
        &mut self,
        dispatch: u64,
        future: u64,
        res: &ResultRecord,
        budget: OccupancyBudget,
    ) -> DispatchOutcome {
        self.slice(budget, Entry::Wake(dispatch, future, res))
    }

    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome {
        // The hint runs on the hibernation path with no dispatch budget of its
        // own; the HTTP budget stops a guest turning it into free compute.
        self.run_gc_hint(pressure, OccupancyBudget::default())
    }

    /// A guest that busy-waits never returns, so its frame never leaves the
    /// stack and it is never a capture candidate; it dies at its occupancy budget.
    fn is_parked(&self) -> bool {
        self.discarded.is_none()
            && self.live.store.data().wasm_depth == 0
            && self.last == Some(Completion::Parked)
    }

    fn is_capture_eligible(&self) -> bool {
        self.discarded.is_none()
            && self.live.store.data().wasm_depth == 0
            && self.last.is_some_and(|c| c.instance_survives())
    }

    fn storage_failed(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self
            .live
            .storage
            .as_ref()
            .is_some_and(crate::storage_fault::Watch::faulted)
        {
            return true;
        }
        self.template.storage_failed()
    }

    fn memory(&mut self) -> &mut [u8] {
        let live = &mut self.live;
        live.memory.data_mut(&mut live.store)
    }

    fn memory_len(&self) -> usize {
        self.live.memory.data_size(&self.live.store)
    }

    fn changed_ranges(&mut self) -> Option<Vec<std::ops::Range<u64>>> {
        // A layered template maps its memory privately from the layer file, so
        // a page the guest never wrote still reads the template.
        #[cfg(target_os = "linux")]
        if self.template.initialization_layer.is_some() {
            let live = &self.live;
            let address = live.memory.data_ptr(&live.store) as usize;
            let length_bytes = live.memory.data_size(&live.store);
            match crate::pagemap::ranges(address, length_bytes, crate::pagemap::private_copy) {
                Ok(ranges) => return Some(ranges),
                Err(error) => tracing::warn!(
                    %error,
                    "reading guest page-table state failed; comparing every page with the template"
                ),
            }
        }
        None
    }

    fn capture_mutable_state(&mut self) -> Result<InstanceImage> {
        crate::capture::capture_mutable_state(self)
    }

    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()> {
        crate::capture::restore_mutable_state(self, img)
    }
}
