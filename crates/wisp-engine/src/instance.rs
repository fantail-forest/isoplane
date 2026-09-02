//! A live activation: the template mapped copy-on-write, its mutable globals
//! and tables, and the tombstone.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use wasmtime::error::Context as WtContext;
use wasmtime::{Global, Memory, Ref, Store, Table, TypedFunc, Val};

use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::{PollStatus, TrapCode};

use crate::budget::{CpuBudget, KillPhase, TrapKind};
use crate::meter::try_thread_cpu_nanos;
use crate::snapshot::GlobalValue;
use crate::state::{
    AllocFailed, BudgetExceeded, GuestAbort, Kill, Meter, Reentrancy, StoreState, WallCapExceeded,
};
use crate::template::TemplateInner;
use crate::{
    Completion, DispatchOutcome, EngineInner, InstanceImage, InstanceOps, Result, TrapInfo,
};

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

enum Entry<'a> {
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

        // The watchdog only ticks while something is hot, so this precedes
        // arming the deadline.
        engine.watchdog.shared().enter_hot();

        let wall0 = Instant::now();
        // `None` rather than `0` on a failed read: `end - 0` would attribute this
        // dispatch the thread's whole lifetime.
        let cpu0 = try_thread_cpu_nanos();
        live.store.data_mut().begin_slice(
            clock_ms,
            Meter {
                cpu0_ns: cpu0.unwrap_or(0),
                wall0: Some(wall0),
                ..meter
            },
        );
        live.store.set_epoch_deadline(meter.check_ticks);

        let raw = call_entry(live, entry);

        // Metered before classification and teardown, so those are not included.
        // Either end unreadable yields 0, a floor, with the failure counted in
        // `meter::clock_read_failures`.
        let cpu_ns = match (cpu0, try_thread_cpu_nanos()) {
            (Some(start), Some(end)) => end.saturating_sub(start),
            _ => 0,
        };
        let wall_ns = wall0.elapsed().as_nanos() as u64;

        live.store.set_epoch_deadline(u64::MAX / 2);
        engine.watchdog.shared().exit_hot();

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
            wall_ns,
            trap,
            kill,
            overage_ns,
            grace_deferrals,
        }
    }
}

fn call_entry(live: &mut Live, entry: Entry<'_>) -> wasmtime::Result<u64> {
    match entry {
        Entry::Handle(rec) => {
            let bytes = postcard::to_allocvec(rec)
                .map_err(wasmtime::Error::new)
                .context("encoding dispatch record")?;
            let (ptr, len) = handoff(live, &bytes)?;
            let f = live.f_handle.clone();
            Ok(f.call(&mut live.store, (ptr, len))? as u64)
        }
        Entry::Wake(future, res) => {
            let bytes = postcard::to_allocvec(res)
                .map_err(wasmtime::Error::new)
                .context("encoding result record")?;
            let (ptr, len) = handoff(live, &bytes)?;
            let f = live.f_wake.clone();
            Ok(f.call(&mut live.store, (future as i64, ptr, len))? as u64)
        }
        Entry::GcHint(pressure) => {
            if let Some(f) = live.f_gc_hint.clone() {
                f.call(&mut live.store, pressure as i32)?;
            }
            Ok(PollStatus::Done { response: 0 }.encode())
        }
        Entry::Initialize => {
            if let Some(f) = live.f_initialize.clone() {
                f.call(&mut live.store, ())?;
            }
            Ok(PollStatus::Done { response: 0 }.encode())
        }
    }
}

/// Host-to-guest data enters via `wisp_alloc` plus a copy; the guest owns the
/// allocation.
fn handoff(live: &mut Live, bytes: &[u8]) -> wasmtime::Result<(i32, i32)> {
    let len = i32::try_from(bytes.len())
        .map_err(wasmtime::Error::new)
        .context("record too large for the guest ABI")?;
    let alloc = live.f_alloc.clone();
    let ptr = alloc.call(&mut live.store, (len, 8))?;
    if ptr == 0 {
        return Err(AllocFailed { size: bytes.len() }.into());
    }
    let mem = live.memory;
    let data = mem.data_mut(&mut live.store);
    let start = ptr as u32 as usize;
    let end = WtContext::context(
        start.checked_add(bytes.len()),
        "wisp_alloc pointer overflow",
    )?;
    WtContext::context(
        data.get_mut(start..end),
        "wisp_alloc returned a pointer outside linear memory",
    )?
    .copy_from_slice(bytes);
    Ok((ptr, len))
}

/// The engine's own reasons are recorded in the store rather than recovered
/// from the error chain, so budget kills and capability violations are
/// classified exactly instead of by matching wasmtime's message strings.
fn classify(live: &Live, err: &wasmtime::Error) -> TrapInfo {
    let d = live.store.data();

    if let Some(kill) = d.kill {
        let kind = TrapKind::BudgetExceeded;
        let message = match kill {
            Kill::CpuHard => err
                .downcast_ref::<BudgetExceeded>()
                .map(|b| b.to_string())
                .unwrap_or_else(|| "cpu budget exceeded".into()),
            Kill::Wall => err
                .downcast_ref::<WallCapExceeded>()
                .map(|w| format!("{w} (wall-clock backstop, not a CPU meter)"))
                .unwrap_or_else(|| "wall-clock backstop tripped".into()),
        };
        return TrapInfo { kind, message };
    }

    if let Some(v) = err.downcast_ref::<crate::state::CapabilityViolation>() {
        return TrapInfo {
            kind: TrapKind::CapabilityViolation,
            message: v.to_string(),
        };
    }
    if let Some(a) = err.downcast_ref::<GuestAbort>() {
        return TrapInfo {
            kind: TrapKind::GuestPanic,
            message: a.to_string(),
        };
    }
    if let Some(a) = err.downcast_ref::<AllocFailed>() {
        return TrapInfo {
            kind: TrapKind::OutOfMemory,
            message: a.to_string(),
        };
    }
    if let Some(r) = err.downcast_ref::<Reentrancy>() {
        return TrapInfo {
            kind: TrapKind::Other,
            message: r.to_string(),
        };
    }
    if d.limits.oom {
        return TrapInfo {
            kind: TrapKind::OutOfMemory,
            message: format!("linear memory growth refused at the plan ceiling: {err}"),
        };
    }

    if let Some(t) = err.downcast_ref::<wasmtime::Trap>() {
        use wasmtime::Trap as T;
        let kind = match t {
            // An epoch deadline with no recorded reason was reached outside a
            // slice; still a budget kill from the tenant's point of view.
            T::Interrupt | T::OutOfFuel => TrapKind::BudgetExceeded,
            T::UnreachableCodeReached => TrapKind::GuestPanic,
            T::AllocationTooLarge => TrapKind::OutOfMemory,
            T::StackOverflow
            | T::MemoryOutOfBounds
            | T::HeapMisaligned
            | T::TableOutOfBounds
            | T::IndirectCallToNull
            | T::BadSignature
            | T::IntegerOverflow
            | T::IntegerDivisionByZero
            | T::BadConversionToInteger
            | T::NullReference
            | T::ArrayOutOfBounds
            | T::CastFailure
            | T::AtomicWaitNonSharedMemory => TrapKind::Sandbox,
            _ => TrapKind::Other,
        };
        return TrapInfo {
            kind,
            message: format!("{err}"),
        };
    }

    TrapInfo {
        kind: TrapKind::Other,
        message: format!("{err:#}"),
    }
}

fn trap_kind_of_code(c: TrapCode) -> TrapKind {
    match c {
        TrapCode::Panic => TrapKind::GuestPanic,
        TrapCode::OutOfMemory => TrapKind::OutOfMemory,
        TrapCode::CapabilityViolation => TrapKind::CapabilityViolation,
        TrapCode::Protocol | TrapCode::TooManyFutures | TrapCode::Unknown => TrapKind::Other,
    }
}

fn trap_code_of_kind(k: TrapKind) -> TrapCode {
    match k {
        TrapKind::GuestPanic => TrapCode::Panic,
        TrapKind::OutOfMemory => TrapCode::OutOfMemory,
        TrapKind::CapabilityViolation => TrapCode::CapabilityViolation,
        TrapKind::BudgetExceeded | TrapKind::Tombstoned | TrapKind::Sandbox | TrapKind::Other => {
            TrapCode::Unknown
        }
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

        match crate::instantiate(&self.engine, &self.template, data) {
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
        if let Some(why) = self.tombstone {
            bail!(
                "refusing to capture a tombstoned instance ({why}); its memory is a state no \
                 execution produced"
            );
        }
        let template = self.template.clone();
        let live = anyhow::Context::context(self.live.as_mut(), "instance has no live store")?;

        let mut globals = Vec::with_capacity(live.globals.len());
        for g in &live.globals {
            let v = g.get(&mut live.store);
            globals.push(GlobalValue::of(v, &live.store)?);
        }

        // wasmtime's public API cannot name a funcref by function index, so a
        // table mutation can be detected but not represented in a delta.
        let table_mutations = Vec::new();
        for (i, t) in live.tables.iter().enumerate() {
            let baseline = template
                .table_nullness
                .get(i)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let size = t.size(&live.store);
            let table_name = template
                .tables
                .get(i)
                .map(|(n, _)| n.as_str())
                .unwrap_or("?");
            if size as usize != baseline.len() {
                bail!(
                    "table `{table_name}` grew from {} to {size} entries; table mutations cannot \
                     be represented in a delta",
                    baseline.len()
                );
            }
            for (slot, was_non_null) in baseline.iter().enumerate() {
                let now_non_null = match t.get(&mut live.store, slot as u64) {
                    Some(Ref::Func(f)) => f.is_some(),
                    Some(Ref::Extern(e)) => e.is_some(),
                    Some(_) => true,
                    None => false,
                };
                if now_non_null != *was_non_null {
                    bail!(
                        "table `{table_name}` slot {slot} changed since activation; table \
                         mutations cannot be represented in a delta"
                    );
                }
            }
        }

        Ok(InstanceImage {
            globals,
            table_mutations,
            memory_pages: live.memory.size(&live.store) as u32,
        })
    }

    fn restore_mutable_state(&mut self, img: &InstanceImage) -> Result<()> {
        if self.tombstone.is_some() {
            bail!("refusing to restore into a tombstoned instance");
        }
        if !img.table_mutations.is_empty() {
            bail!("table mutations cannot be applied: wasmtime exposes no function-index to funcref mapping");
        }
        let live = anyhow::Context::context(self.live.as_mut(), "instance has no live store")?;

        // Grow first, so the delta pages `wisp-hibernate` writes through
        // `memory()` have somewhere to land.
        let cur = live.memory.size(&live.store) as u32;
        if img.memory_pages > cur {
            live.memory
                .grow(&mut live.store, (img.memory_pages - cur) as u64)
                .map_err(anyhow::Error::from)
                .context("growing linear memory to the captured size")?;
        }

        if img.globals.len() != live.globals.len() {
            bail!(
                "delta carries {} globals but this template has {}; a delta is only valid \
                 against its own module hash and ABI rev",
                img.globals.len(),
                live.globals.len()
            );
        }
        for (g, v) in live.globals.iter().zip(&img.globals) {
            g.set(&mut live.store, Val::from(*v))
                .map_err(anyhow::Error::from)
                .context("restoring a mutable global")?;
        }
        Ok(())
    }
}
