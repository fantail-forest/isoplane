//! Guest activation state and synchronous execution.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use wasmtime::error::Context as WtContext;
use wasmtime::{Global, Memory, Ref, Store, Table, TypedFunc, Val};

use wisp_abi::dispatch::DispatchRecord;
use wisp_abi::result::ResultRecord;
use wisp_abi::status::{PollStatus, TrapCode};
use wisp_abi::PAGE_SIZE;

use crate::budget::{CpuBudget, KillPhase, TrapKind};
use crate::meter::try_thread_cpu_nanos;
use crate::state::{
    AllocFailed, BudgetExceeded, GuestAbort, Kill, Meter, Reentrancy, StoreState, WallCapExceeded,
};
use crate::template::TemplateInner;
use crate::{
    Completion, DispatchOutcome, EngineInner, InstanceImage, InstanceOps, Result, TrapInfo,
};

/// The wasmtime side of an activation. Separated from [`Instance`] so that
/// `reset` can drop the whole thing — returning the slot to the pool, which is
/// what makes reset a `madvise(MADV_DONTNEED)` rather than a memset — and build
/// a fresh one while keeping the caller's `Instance` handle valid.
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
    /// Reason further execution is prohibited after a trap or hard budget kill.
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

/// What the engine is about to call into the guest.
enum Entry<'a> {
    Handle(&'a DispatchRecord),
    Wake(u64, &'a ResultRecord),
    GcHint(u8),
    Initialize,
}

impl Instance {
    /// Estimate from engine allocation constants and host Rust value sizes; excludes heap allocations.
    pub fn bookkeeping_bytes(&self) -> usize {
        let committed = crate::wtconfig::BOOKKEEPING_BYTES;
        let host = std::mem::size_of::<Instance>()
            + std::mem::size_of::<Live>()
            + std::mem::size_of::<StoreState>()
            + self
                .live
                .as_ref()
                .map(|l| {
                    l.store.data().scratch.capacity()
                        + l.globals.len() * std::mem::size_of::<Global>()
                        + l.tables.len() * std::mem::size_of::<Table>()
                })
                .unwrap_or(0);
        committed + host
    }

    /// Counts pages that differ from the template; reading the full mapping can make it resident.
    pub fn dirty_page_count(&self) -> usize {
        let Some(live) = self.live.as_ref() else {
            return 0;
        };
        let data = live.memory.data(&live.store);
        let image = &self.template.image;
        let mut n = 0;
        let pages = data.len() / PAGE_SIZE;
        for p in 0..pages {
            let cur = &data[p * PAGE_SIZE..(p + 1) * PAGE_SIZE];
            let base = image
                .get(p * PAGE_SIZE..(p + 1) * PAGE_SIZE)
                .unwrap_or(&ZERO_PAGE);
            if cur != base {
                n += 1;
            }
        }
        n
    }

    /// Whether a trap or hard budget kill has prohibited further execution.
    pub fn is_poisoned(&self) -> bool {
        self.tombstone.is_some()
    }

    /// Recorded reason further execution is prohibited, if any.
    pub fn tombstone_reason(&self) -> Option<&'static str> {
        self.tombstone
    }

    /// Panics when attempting to enter an instance already marked as terminated.
    #[inline]
    fn assert_not_tombstoned(&self, what: &str) {
        if let Some(why) = self.tombstone {
            panic!(
                "call into a TOMBSTONED instance: `{what}` on an instance discarded because {why}"
            );
        }
    }

    /// How the last dispatch ended, or `None` if none has run since activation
    /// or reset.
    pub fn last_completion(&self) -> Option<Completion> {
        self.last
    }

    /// Import responsible for the last capability violation, if recorded.
    pub fn last_capability_violation(&self) -> Option<&'static str> {
        self.last_violation
    }

    /// Maximum Wasm call depth observed by the engine call hook.
    pub fn max_wasm_depth_observed(&self) -> i32 {
        self.live
            .as_ref()
            .map(|l| l.store.data().max_wasm_depth)
            .unwrap_or(0)
    }

    /// Runs guest initialization with the supplied CPU budget.
    pub fn run_initialize(&mut self, budget: CpuBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Initialize)
    }

    // -- internals ----------------------------------------------------------

    fn refuse(&self, why: &str) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Trapped {
                code: TrapCode::Unknown,
            },
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some(TrapInfo {
                kind: TrapKind::Other,
                message: why.to_string(),
            }),
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    /// Executes one guest entry while measuring its CPU consumption.
    fn slice(&mut self, budget: CpuBudget, entry: Entry<'_>) -> DispatchOutcome {
        self.assert_not_tombstoned(match entry {
            Entry::Handle(_) => "wisp_handle",
            Entry::Wake(..) => "wisp_wake",
            Entry::GcHint(_) => "wisp_gc_hint",
            Entry::Initialize => "wisp_initialize",
        });
        if self.live.is_none() {
            return self.refuse("instance has no live store");
        }

        let engine = self.engine.clone();
        let live = self.live.as_mut().expect("checked above");

        // Reentrant entry would corrupt per-slice clock and measurement state.
        let depth = live.store.data().wasm_depth;
        if depth != 0 {
            let e: anyhow::Error = Reentrancy { depth }.into();
            return DispatchOutcome {
                status: PollStatus::Trapped {
                    code: TrapCode::Protocol,
                },
                completion: Completion::Trapped,
                cpu_ns: 0,
                wall_ns: 0,
                trap: Some(TrapInfo {
                    kind: TrapKind::Other,
                    message: format!("{e}"),
                }),
                kill: None,
                overage_ns: 0,
                grace_deferrals: 0,
            };
        }

        // Sample the guest clock at slice entry.
        let clock_ms = live
            .store
            .data()
            .bindings
            .as_ref()
            .map(|b| b.slice_clock_ms())
            .unwrap_or(0);

        let meter = Meter {
            cpu0_ns: 0, // filled in below, as late as possible
            wall0: None,
            cpu_budget_ns: budget.budget_ns(),

            grace_ns: budget.grace_ns(),
            spent_ns: budget.spent_ns,
            wall_cap_ns: (budget.wall_ms_cap as u64).saturating_mul(1_000_000),
            // Check the thread CPU clock on each epoch callback.
            check_ticks: 1,
        };

        // A successful GC hint preserves the preceding completion state for capture.
        let is_gc_hint = matches!(entry, Entry::GcHint(_));

        // Compare the epoch interval with the allowance used by this dispatch.
        debug_assert!(
            budget.grace_ns() / engine.cfg.epoch_tick_us.max(1).saturating_mul(1_000)
                >= crate::budget::MIN_GRACE_TICKS,
            "epoch_tick_us = {} leaves fewer than {} ticks inside the {:.1} ms grace allowance",
            engine.cfg.epoch_tick_us,
            crate::budget::MIN_GRACE_TICKS,
            budget.grace_ns() as f64 / 1e6,
        );

        // The watchdog only ticks while something is Hot, so this must precede
        // arming the deadline or the deadline would never be reached.
        engine.watchdog.shared().enter_hot();

        let wall0 = Instant::now();
        // A failed clock read must not be mistaken for a zero starting value.
        let cpu0 = try_thread_cpu_nanos();
        live.store.data_mut().begin_slice(
            clock_ms,
            // The budget path keeps today's behaviour on an unreadable clock
            // (`cpu0_ns: 0`, so `dispatch_cpu_ns` reads high and the dispatch
            // is killed at its deadline rather than escaping its budget). That
            // is the conservative direction, and the counter now says it
            // happened; accounting is what this change makes honest.
            Meter {
                cpu0_ns: cpu0.unwrap_or(0),
                wall0: Some(wall0),
                ..meter
            },
        );
        live.store.set_epoch_deadline(meter.check_ticks);

        let raw = call_entry(live, entry);

        // Metered before anything else, so that trap classification and
        // teardown are not included in the guest CPU interval.
        //
        // Either end unreadable yields `0` — a floor, never an invention —
        // and `meter::clock_read_failures()` is non-zero, so a meter
        // cross-check can attribute its residual instead of absorbing it into
        // "platform overhead".
        let cpu_ns = match (cpu0, try_thread_cpu_nanos()) {
            (Some(start), Some(end)) => end.saturating_sub(start),
            _ => 0,
        };
        let wall_ns = wall0.elapsed().as_nanos() as u64;

        // Stand the deadline down: host-side work on this store afterwards
        // (capture, reset, a `Global::get`) must not inherit a live deadline.
        live.store.set_epoch_deadline(u64::MAX / 2);
        engine.watchdog.shared().exit_hot();

        let (status, trap) = match raw {
            Ok(v) => {
                let status = PollStatus::decode(v);
                let trap = match status {
                    // The guest reported its own fault through the ABI rather
                    // than trapping. Same disposition, different provenance.
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

        // A parked slice preserves its consumed CPU allowance for its next wake.
        let hard_kill = matches!(
            trap.as_ref().map(|t| t.kind),
            Some(TrapKind::BudgetExceeded)
        );
        let dispatch_cpu = budget.spent_ns.saturating_add(cpu_ns);
        // A dispatch is in grace if it crossed B — whether or not the epoch
        // callback got there first.
        //
        // The guest can return after exceeding its budget before the next epoch callback.
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
            // Grace was entered but the dispatch has not ended yet (parked, or
            // it trapped for an unrelated reason). No phase to report.
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
        // Prevent a trapped store from being entered again.
        if completion.tombstones() {
            self.tombstone = Some(match completion {
                Completion::BudgetKill => {
                    "execution reached the hard CPU deadline and trapped without unwinding"
                }
                _ => "execution trapped; the same instance cannot run again",
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

const ZERO_PAGE: [u8; PAGE_SIZE] = [0u8; PAGE_SIZE];

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
            // The GC hint export is optional.
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

/// Allocates guest memory and copies the input bytes into it.
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

/// Classifies dispatch failure using recorded engine state and the Wasmtime error.
fn classify(live: &Live, err: &wasmtime::Error) -> TrapInfo {
    let d = live.store.data();

    if let Some(kill) = d.kill {
        let kind = TrapKind::BudgetExceeded;
        let message = match kill {
            Kill::CpuHard => err
                .downcast_ref::<BudgetExceeded>()
                .map(|b| b.to_string())
                .unwrap_or_else(|| "cpu budget exceeded".into()),
            // The wall-clock backstop is distinct from the CPU budget, but the
            // tenant-facing outcome is the same abrupt, un-unwound termination,
            // so it carries the same code with a message that says which limit.
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
            // Epoch deadline with no recorded reason: the deadline was reached
            // outside a slice, or the callback was not armed. Still a budget
            // kill from the tenant's point of view.
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
        // These host-side trap categories have no more specific guest ABI code.
        TrapKind::BudgetExceeded | TrapKind::Sandbox | TrapKind::Other => TrapCode::Unknown,
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
        // The hint runs on the hibernation path, not a request path, so it has
        // no dispatch budget of its own; the HTTP budget is a generous cap that
        // still stops a guest turning `gc-hint` into free compute.
        self.slice(CpuBudget::http(), Entry::GcHint(pressure))
    }

    /// Recreates the live store from the prepared template; refuses terminated instances.
    fn reset(&mut self) -> Result<()> {
        if let Some(why) = self.tombstone {
            bail!("refusing to reset a TOMBSTONED instance ({why}); activate a fresh instance");
        }
        let live = anyhow::Context::context(self.live.take(), "instance has no live store")?;
        let mut data = live.store.into_data();
        data.memory = None;
        data.wasm_depth = 0;
        data.max_wasm_depth = 0;
        data.kill = None;
        data.grace_at_ns = None;
        data.unsafe_to_trap = false;
        data.grace_deferrals = 0;
        data.violation = None;
        data.abort_msg = None;
        data.limits.oom = false;
        data.scratch.clear();

        self.live = Some(crate::instantiate(&self.engine, &self.template, data)?);
        self.last = None;
        self.last_violation = None;
        Ok(())
    }

    /// Reports a live, nonterminated instance with an empty Wasm stack whose last completion was parked.
    fn is_parked(&self) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        self.tombstone.is_none()
            && live.store.data().wasm_depth == 0
            && self.last == Some(Completion::Parked)
    }

    /// Checks live stack depth and completion state before capture.
    fn is_capture_eligible(&self) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };

        self.tombstone.is_none()
            && live.store.data().wasm_depth == 0
            && self.last.is_some_and(|c| c.instance_survives())
    }

    fn memory(&mut self) -> &mut [u8] {
        let live = self.live.as_mut().expect("instance has no live store");
        let mem = live.memory;
        mem.data_mut(&mut live.store)
    }

    fn memory_len(&self) -> usize {
        self.live
            .as_ref()
            .map(|l| l.memory.data_size(&l.store))
            .unwrap_or(0)
    }

    fn capture_mutable_state(&mut self) -> Result<InstanceImage> {
        if let Some(why) = self.tombstone {
            bail!("refusing to capture a TOMBSTONED instance ({why})");
        }
        let template = self.template.clone();
        let live = anyhow::Context::context(self.live.as_mut(), "instance has no live store")?;

        let mut globals = Vec::with_capacity(live.globals.len());
        for g in &live.globals {
            let v = g.get(&mut live.store);
            globals.push(to_global_value(v, &live.store)?);
        }

        // Tables. See the note on `TABLE GAP` below: the engine detects
        // mutation but cannot represent it, so it fails loudly rather than
        // dropping half of invariant 1 on the floor.
        let table_mutations = Vec::new();
        for (i, t) in live.tables.iter().enumerate() {
            let baseline = template
                .table_nullness
                .get(i)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let size = t.size(&live.store);
            if size as usize != baseline.len() {
                bail!(
                    "table `{}` grew from {} to {size} entries; the engine cannot name a funcref \
                     by function index through wasmtime's public API, so this mutation cannot be \
                     represented in a delta (documented gap)",
                    template
                        .tables
                        .get(i)
                        .map(|(n, _)| n.as_str())
                        .unwrap_or("?"),
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
                        "table `{}` slot {slot} changed nullness since activation; see the \
                         documented gap on funcref identity",
                        template
                            .tables
                            .get(i)
                            .map(|(n, _)| n.as_str())
                            .unwrap_or("?")
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
            bail!(
                "table mutations cannot be applied: wasmtime exposes no function-index -> funcref \
                 mapping, so a captured mutation could not be reproduced (documented gap)"
            );
        }
        let live = anyhow::Context::context(self.live.as_mut(), "instance has no live store")?;

        // Grow memory before restoring captured pages.
        let cur = live.memory.size(&live.store) as u32;
        if img.memory_pages > cur {
            live.memory
                .grow(&mut live.store, (img.memory_pages - cur) as u64)
                .map_err(anyhow::Error::from)
                .context("growing linear memory to the captured size")?;
        }

        if img.globals.len() != live.globals.len() {
            bail!(
                "delta carries {} globals but this template has {}",
                img.globals.len(),
                live.globals.len()
            );
        }
        for (g, v) in live.globals.iter().zip(&img.globals) {
            g.set(&mut live.store, from_global_value(*v))
                .map_err(anyhow::Error::from)
                .context("restoring a mutable global")?;
        }
        Ok(())
    }
}

fn to_global_value(
    v: Val,
    store: impl wasmtime::AsContext,
) -> Result<crate::snapshot::GlobalValue> {
    use crate::snapshot::GlobalValue as G;
    Ok(match v {
        Val::I32(x) => G::I32(x),
        Val::I64(x) => G::I64(x),
        Val::F32(x) => G::F32(x),
        Val::F64(x) => G::F64(x),
        other => bail!(
            "mutable global of type {:?} is outside the numeric set invariant 1 can capture",
            other.ty(store)
        ),
    })
}

fn from_global_value(v: crate::snapshot::GlobalValue) -> Val {
    use crate::snapshot::GlobalValue as G;
    match v {
        G::I32(x) => Val::I32(x),
        G::I64(x) => Val::I64(x),
        G::F32(x) => Val::F32(x),
        G::F64(x) => Val::F64(x),
    }
}
