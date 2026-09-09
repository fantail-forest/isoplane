//! A live activation: the template mapped copy-on-write, its mutable globals
//! and tables, and the tombstone.

use std::sync::Arc;
use std::time::Instant;

use anyhow::bail;
use wasmtime::{Global, Memory, Store, Table, TypedFunc};

use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::PollStatus;

use crate::budget::{CpuBudget, CpuMeasurement, ExecutionLimit, KillPhase, TrapKind};
use crate::call::{call_entry, classify, trap_code_of_kind, trap_kind_of_code};
use crate::meter::interval_cpu_ns;
use crate::state::{Meter, Reentrancy, StoreState};
use crate::template::TemplateInner;
use crate::{DispatchOutcome, EngineInner, InstanceImage, Result, TrapInfo};

/// The wasmtime side of an activation. Separate from [`Instance`] so that
/// `reset` can drop the whole thing, returning the slot to the pool, and build
/// a fresh one while the caller's `Instance` stays valid.
pub(crate) struct Live {
    pub(crate) store: Store<StoreState>,
    pub(crate) memory: Memory,
    pub(crate) f_alloc: TypedFunc<(i32, i32), i32>,
    pub(crate) f_handle: TypedFunc<(i32, i32), i64>,
    pub(crate) f_wake: TypedFunc<(i64, i32, i32), i64>,
    pub(crate) f_gc_hint: Option<TypedFunc<i32, ()>>,
    pub(crate) f_initialize: Option<TypedFunc<(), ()>>,
    pub(crate) globals: Vec<Global>,
    pub(crate) tables: Vec<Table>,
}

pub struct Instance {
    pub(crate) engine: Arc<EngineInner>,
    pub(crate) template: Arc<TemplateInner>,
    pub(crate) live: Option<Live>,
    /// Set when a call ended in a way [`Completion::tombstones`] says leaves
    /// the instance inconsistent. wasmtime does not poison a store after a
    /// trap, so every entry point checks this and refuses.
    pub(crate) tombstone: Option<&'static str>,
    pub(crate) last: Option<Completion>,
    pub(crate) last_violation: Option<&'static str>,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("template", &self.template.memory_pages)
            .field("tombstone", &self.tombstone)
            .field("last", &self.last)
            .field("live", &self.live.is_some())
            .finish()
    }
}

pub(crate) enum Entry<'a> {
    Handle(&'a DispatchRecord),
    Wake(u64, &'a ResultRecord),
    GcHint(u8),
    Initialize,
}

impl Entry<'_> {
    fn export_name(&self) -> &'static str {
        match self {
            Entry::Handle(_) => "wisp_handle",
            Entry::Wake(..) => "wisp_wake",
            Entry::GcHint(_) => "wisp_gc_hint",
            Entry::Initialize => "wisp_initialize",
        }
    }
}

struct HotInvocation<'a>(&'a crate::watchdog::Shared);

impl<'a> HotInvocation<'a> {
    fn new(watchdog: &'a crate::watchdog::Shared) -> Self {
        watchdog.enter_hot();
        Self(watchdog)
    }
}

impl Drop for HotInvocation<'_> {
    fn drop(&mut self) {
        self.0.exit_hot();
    }
}

impl Instance {
    /// The import that tripped capability policy on the last call, if any.
    /// Pre-initialisation identifies an impure static initialiser by which
    /// forbidden import it touched.
    pub fn last_capability_violation(&self) -> Option<&'static str> {
        self.last_violation
    }

    /// Run `wisp_initialize`, for pre-initialisation with bindings whose
    /// `is_init_sandbox()` is true. Ordinary activation never calls this; the
    /// template already ran it.
    pub fn run_initialize(&mut self, budget: CpuBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Initialize)
    }

    fn refuse(&self, kind: TrapKind, why: String) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Trapped {
                code: trap_code_of_kind(kind),
            },
            completion: Completion::Trapped,
            cpu_ns: 0,
            cpu_measurement: CpuMeasurement::NotExecuted,
            execution_limit: None,
            wall_ns: 0,
            trap: Some(TrapInfo { kind, message: why }),
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    /// One hot interval: meter start, epoch deadline armed, synchronous call
    /// into the guest, meter stop.
    fn slice(&mut self, budget: CpuBudget, entry: Entry<'_>) -> DispatchOutcome {
        if let Some(why) = self.tombstone {
            return self.refuse(
                TrapKind::Tombstoned,
                format!(
                    "`{}` called on a tombstoned instance, discarded because {why}",
                    entry.export_name()
                ),
            );
        }
        if self.live.is_none() {
            return self.refuse(TrapKind::Other, "instance has no live store".to_string());
        }

        let engine = self.engine.clone();
        let live = self.live.as_mut().expect("checked above");

        // A reentrant call would corrupt the meter and the frozen clock.
        let depth = live.store.data().wasm_depth;
        if depth != 0 {
            let e: anyhow::Error = Reentrancy { depth }.into();
            return self.refuse(TrapKind::Other, format!("{e}"));
        }

        let clock_ms = live
            .store
            .data()
            .bindings
            .as_ref()
            .map(|b| b.slice_clock_ms())
            .unwrap_or(0);

        let meter = Meter {
            cpu0_ns: 0,
            wall0: None,
            cpu_budget_ns: budget.budget_ns(),
            grace_ns: budget.grace_ns(),
            spent_ns: budget.spent_ns,
            wall_cap_ns: (budget.wall_ms_cap as u64).saturating_mul(1_000_000),
            check_ticks: 1,
        };

        // A gc hint runs on a parked instance immediately before capture and
        // must not clear the park state.
        let is_gc_hint = matches!(entry, Entry::GcHint(_));

        let cpu0 = match crate::meter::thread_cpu_nanos() {
            Ok(cpu0) => cpu0,
            Err(error) => return self.refuse(TrapKind::CpuClockUnavailable, error.to_string()),
        };

        // The watchdog only ticks while something is hot, so this precedes
        // arming the deadline.
        let hot = HotInvocation::new(engine.watchdog.shared());

        let wall0 = Instant::now();
        live.store.data_mut().begin_slice(
            clock_ms,
            Meter {
                cpu0_ns: cpu0,
                wall0: Some(wall0),
                ..meter
            },
        );
        live.store.set_epoch_deadline(meter.check_ticks);

        let raw = call_entry(live, entry);

        // Classification and teardown are outside the invocation's CPU meter.
        let cpu_end = interval_cpu_ns(cpu0);
        let (cpu_ns, cpu_measurement) = match cpu_end {
            Some(cpu_ns) => (cpu_ns, CpuMeasurement::Complete),
            None => (0, CpuMeasurement::Incomplete),
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

        let violation = live.store.data().violation;
        let grace_at_ns = live.store.data().grace_at_ns;
        let grace_deferrals = live.store.data().grace_deferrals;

        // A `BudgetExceeded` trap comes only from the hard deadline. A slice
        // that entered grace and returned `done` is a clean kill; one that
        // returned `pending` parked at an await and the wake that follows
        // carries `spent_ns`, so it is not classified yet.
        let hard_kill = matches!(
            trap.as_ref().map(|t| t.kind),
            Some(TrapKind::BudgetExceeded)
        );
        let dispatch_cpu = budget.spent_ns.saturating_add(cpu_ns);
        // A guest can return between epoch ticks; compare its active execution time
        // with the budget even when the epoch callback did not run.
        let in_grace = grace_at_ns.is_some() || dispatch_cpu >= budget.budget_ns();
        let completion = if hard_kill {
            Completion::BudgetKill
        } else {
            match Completion::from_status(status) {
                Completion::Done if in_grace => Completion::CleanKill,
                other => other,
            }
        };
        let kill = match (hard_kill, completion) {
            (true, _) => Some(KillPhase::Hard),
            (false, Completion::CleanKill) => Some(KillPhase::Clean),
            _ => None,
        };
        let overage_ns = if in_grace || hard_kill {
            dispatch_cpu.saturating_sub(budget.budget_ns())
        } else {
            0
        };

        if !(is_gc_hint && completion.instance_survives()) {
            self.last = Some(completion);
        }
        self.last_violation = violation;
        if completion.tombstones() {
            self.tombstone = Some(match completion {
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
            cpu_ns,
            cpu_measurement,
            execution_limit: match live.store.data().kill {
                Some(crate::state::Kill::CpuHard) => Some(ExecutionLimit::Cpu),
                Some(crate::state::Kill::Wall) => Some(ExecutionLimit::Wall),
                None if completion == Completion::CleanKill => Some(ExecutionLimit::Cpu),
                None => None,
            },
            wall_ns,
            trap,
            kill,
            overage_ns,
            grace_deferrals,
        }
    }
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

impl InstanceOps for Instance {
    fn dispatch(&mut self, rec: &DispatchRecord, budget: CpuBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Handle(rec))
    }

    fn wake(&mut self, future: u64, res: &ResultRecord, budget: CpuBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Wake(future, res))
    }

    fn gc_hint(&mut self, pressure: u8) -> DispatchOutcome {
        // The hint runs on the hibernation path with no dispatch budget of its
        // own; the HTTP budget stops a guest turning it into free compute.
        self.slice(CpuBudget::http(), Entry::GcHint(pressure))
    }

    fn reset(&mut self) -> Result<()> {
        if let Some(why) = self.tombstone {
            bail!(
                "refusing to reset a tombstoned instance ({why}); drop it and activate a fresh one"
            );
        }
        let live = anyhow::Context::context(self.live.take(), "instance has no live store")?;
        let mut data = live.store.into_data();
        data.memory = None;
        data.wasm_depth = 0;
        data.kill = None;
        data.grace_at_ns = None;
        data.unsafe_to_trap = false;
        data.grace_deferrals = 0;
        data.violation = None;
        data.abort_msg = None;
        data.limits.oom = false;
        data.scratch.clear();

        match crate::activate::instantiate(&self.engine, &self.template, data) {
            Ok(live) => self.live = Some(live),
            Err(e) => {
                self.tombstone = Some("the template could not be re-instantiated after a reset");
                return Err(e);
            }
        }
        self.last = None;
        self.last_violation = None;
        Ok(())
    }

    /// A guest that busy-waits never returns, so its frame never leaves the
    /// stack and it is never a capture candidate; it dies at its CPU budget.
    fn is_parked(&self) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        self.tombstone.is_none()
            && live.store.data().wasm_depth == 0
            && self.last == Some(Completion::Parked)
    }

    fn is_capture_eligible(&self) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        self.tombstone.is_none()
            && live.store.data().wasm_depth == 0
            && self.last.is_some_and(|c| c.instance_survives())
    }

    fn memory(&mut self) -> &mut [u8] {
        match self.live.as_mut() {
            Some(live) => {
                let mem = live.memory;
                mem.data_mut(&mut live.store)
            }
            None => &mut [],
        }
    }

    fn memory_len(&self) -> usize {
        self.live
            .as_ref()
            .map(|l| l.memory.data_size(&l.store))
            .unwrap_or(0)
    }

    fn capture_mutable_state(&mut self) -> Result<InstanceImage> {
        crate::capture::capture_mutable_state(self)
    }

    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()> {
        crate::capture::restore_mutable_state(self, img)
    }
}
