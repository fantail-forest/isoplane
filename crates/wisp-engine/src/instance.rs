//! A live activation: template mapped copy-on-write + mutable globals +
//! tables + handle set.

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
use crate::{Completion, DispatchOutcome, EngineInner, InstanceImage, InstanceOps, Result, TrapInfo};

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
    /// **The tombstone.**
    ///
    /// Set the moment a dispatch ends in a way [`Completion::tombstones`] says
    /// leaves the instance internally inconsistent: a hard budget kill or a
    /// trap. The trap lands at an arbitrary back-edge — mid-collection,
    /// mid-queue-mutation, mid-frame — and **wasmtime does not poison the
    /// store afterwards**: this prototype has watched a killed instance answer
    /// a later exported call correctly, globals intact. The engine is
    /// forgiving, so the platform must not be.
    ///
    /// A tombstone is not a soft refusal. Every call into the guest below
    /// **panics**, because reaching one means the host lost track of an
    /// instance it had already been told was dead, and there is no safe answer
    /// to give. `wisp-host` refuses at the slot (`EnterError::Tombstoned`)
    /// long before this fires; this is the backstop that makes that refusal
    /// checkable rather than hopeful.
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
    /// The host-side Rust structures this engine keeps per activation.
    ///
    /// ## What this deliberately no longer includes
    ///
    /// It used to add `wtconfig::BOOKKEEPING_BYTES` — the sum of the pooling
    /// allocator's two per-slot caps — and call the total "fixed bookkeeping
    /// cost per instance". That number was never a cost. Both caps are pool
    /// *strides*, so raising them changes this figure and changes residency
    /// not at all: `tests/bench_density.rs` measures the same per-instance
    /// overhead at either end of the cap range. Over four raises the sum grew
    /// by orders of magnitude while the measured number never moved, so the
    /// reported value tracked a configuration constant and was mistaken for
    /// RAM.
    ///
    /// Reporting a ceiling next to a measurement invites exactly that
    /// confusion, and no amount of labelling fixes a number that is off by
    /// orders of magnitude and moves whenever someone tunes a cap. So the
    /// ceiling is gone rather than annotated, and what is left is a quantity
    /// that really is allocated per instance and really does change when
    /// someone inlines a buffer into `StoreState` or `Live`.
    ///
    /// This still excludes wasmtime's own `Store` heap allocations, which are
    /// not introspectable. The density test measures the ground truth — RSS
    /// growth per instance — and its number, not this one, is the one the
    /// density argument rests on.
    pub fn host_bookkeeping_bytes(&self) -> usize {
        std::mem::size_of::<Instance>()
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
                .unwrap_or(0)
    }

    /// Pages written since activation. The marginal memory cost of this
    /// instance, and the thing hibernation telemetry surfaces to tenants.
    ///
    /// Implemented as the full page-diff against the template image, which is
    /// the fallback for a soft-dirty PTE walk. It is what the hibernation
    /// delta will actually cost, which is the number the telemetry is about; a
    /// PTE walk would additionally count pages written back to their template
    /// value, and would need `/proc/self/clear_refs`, which is process-wide
    /// and would clobber every other instance's tracking.
    ///
    /// It is not cheap — `tests/bench_latency.rs` times it — and it leaves the
    /// whole mapping resident, so the next `reset` has the whole memory to
    /// decommit rather than just the dirty set. That is a hibernation-path and
    /// telemetry price, never a hot-path one, and it is why a soft-dirty PTE
    /// walk is the mechanism to prefer: it was orders of magnitude faster when
    /// measured, on wasmtime's own CoW mapping. Adopting it here needs
    /// process-wide `/proc/self/clear_refs` batching, which is
    /// `wisp-hibernate`'s call to make.
    pub fn dirty_page_count(&self) -> usize {
        let Some(live) = self.live.as_ref() else { return 0 };
        let data = live.memory.data(&live.store);
        let image = &self.template.image;
        let mut n = 0;
        let pages = data.len() / PAGE_SIZE;
        for p in 0..pages {
            // `page_eq` carries the zero-extension rule this loop used to spell
            // out with `ZERO_PAGE`: memory the guest grew into past the end of
            // the template reads as zero by WASM's own rules, so a page beyond
            // the image is compared against zeros rather than treated as an
            // error.
            if !image.page_eq(p, &data[p * PAGE_SIZE..(p + 1) * PAGE_SIZE]) {
                n += 1;
            }
        }
        n
    }

    /// True once a dispatch ended in a way the instance does not survive — a
    /// hard budget kill or a trap. The host must drop it; every call into the
    /// guest from here on is a hard assertion failure.
    pub fn is_poisoned(&self) -> bool {
        self.tombstone.is_some()
    }

    /// Has this instance been tombstoned, and why.
    pub fn tombstone_reason(&self) -> Option<&'static str> {
        self.tombstone
    }

    /// The hard assertion any subsequent host call into a tombstone must hit.
    ///
    /// Called at the top of every entry point. It panics rather than returning
    /// an error on purpose: an `Err` is something a caller can ignore, and the
    /// failure this rule exists to close is exactly a killed instance quietly
    /// answering.
    #[inline]
    fn assert_not_tombstoned(&self, what: &str) {
        if let Some(why) = self.tombstone {
            panic!(
                "call into a TOMBSTONED instance: `{what}` on an instance discarded because {why}. \
                 Any subsequent host call into a hard-killed instance is a hard assertion \
                 failure: its memory is a state no execution ever produced, the engine does not \
                 poison the store for us, and its slot must be reset before reuse."
            );
        }
    }

    /// How the last dispatch ended, or `None` if none has run since activation
    /// or reset.
    pub fn last_completion(&self) -> Option<Completion> {
        self.last
    }

    /// The import that tripped capability policy on the last dispatch, if any.
    /// The pre-init loop needs this: an impure static initializer is
    /// identified by *which* forbidden import it touched.
    pub fn last_capability_violation(&self) -> Option<&'static str> {
        self.last_violation
    }

    /// High-water mark of wasm frames observed on this instance's native stack,
    /// as reported by wasmtime's call hook.
    ///
    /// Two things are checkable with it. It must be ≥ 1 after any dispatch,
    /// which proves the depth counter behind [`InstanceOps::is_parked`] tracks
    /// reality rather than being decoration. And it must never exceed 1, which
    /// is the single-caller rule — the host must not call into an instance
    /// while a previous call into it has not returned — observed from inside.
    pub fn max_wasm_depth_observed(&self) -> i32 {
        self.live.as_ref().map(|l| l.store.data().max_wasm_depth).unwrap_or(0)
    }

    /// Run `wisp_initialize`. Used by `wisp-control`'s pre-init replay
    /// loop, which activates with bindings whose `is_init_sandbox()` is true
    /// and reads back the offending import from
    /// [`Instance::last_capability_violation`].
    ///
    /// Ordinary activation never calls this: the template already ran it, which
    /// is exactly why cold activation skips static init.
    pub fn run_initialize(&mut self, budget: CpuBudget) -> DispatchOutcome {
        self.slice(budget, Entry::Initialize)
    }

    // -- internals ----------------------------------------------------------

    fn refuse(&self, why: &str) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Trapped { code: TrapCode::Unknown },
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some(TrapInfo { kind: TrapKind::Other, message: why.to_string() }),
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    /// One Hot interval: meter start, epoch deadline armed, synchronous
    /// call into the guest, meter stop.
    fn slice(&mut self, budget: CpuBudget, entry: Entry<'_>) -> DispatchOutcome {
        // Any subsequent host call into a tombstoned instance is a hard
        // assertion failure. Not a refusal it could ignore — a panic.
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

        // Enforced here as well as in `wisp-host`: a reentrant call would
        // corrupt the meter and the frozen clock, not merely the guest.
        let depth = live.store.data().wasm_depth;
        if depth != 0 {
            let e: anyhow::Error = Reentrancy { depth }.into();
            return DispatchOutcome {
                status: PollStatus::Trapped { code: TrapCode::Protocol },
                completion: Completion::Trapped,
                cpu_ns: 0,
                wall_ns: 0,
                trap: Some(TrapInfo { kind: TrapKind::Other, message: format!("{e}") }),
                kill: None,
                overage_ns: 0,
                grace_deferrals: 0,
            };
        }

        // The clock is sampled once, here, and frozen for the slice. It
        // advances again only at a host-call boundary (a future-returning
        // import) or at the next wake.
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
            // G = max(2 ms, 10% of B). Computed from the budget rather than
            // configured, because it is a property of B, not a knob.
            grace_ns: budget.grace_ns(),
            spent_ns: budget.spent_ns,
            wall_cap_ns: (budget.wall_ms_cap as u64).saturating_mul(1_000_000),
            // Check the thread clock at every epoch tick, so the tick is the
            // granularity of the budget kill and the cost is one vDSO
            // `clock_gettime` per tick of guest execution.
            check_ticks: 1,
        };

        // `gc_hint` is called on a **Parked** instance, immediately
        // before hibernation capture. It is not a dispatch and must not clear
        // the park state — otherwise the call that prepares an instance for
        // capture would be the call that makes it ineligible.
        let is_gc_hint = matches!(entry, Entry::GcHint(_));

        // Grace is *delivered* by the epoch counter, so a tick coarser than
        // the grace window is a window the platform cannot open. Asserted
        // where the budget and the engine's tick actually meet — a dispatch —
        // rather than at engine construction, because a rig that builds
        // engines only to measure the watchdog by differencing tick rates is a
        // legitimate coarse-tick caller and never gets here.
        //
        // The failure this guards is silent in the worst way: nothing errors,
        // the deadline still extends to B+G, and every over-budget dispatch
        // simply takes the hard trap having been offered nothing. What an
        // operator sees is a clean:hard ratio of zero, which reads as "our
        // tenants stopped cooperating" — with the constant that caused it
        // nowhere near that dashboard.
        debug_assert!(
            budget.grace_ns() / engine.cfg.epoch_tick_us.max(1).saturating_mul(1_000)
                >= crate::budget::MIN_GRACE_TICKS,
            "epoch_tick_us = {} leaves fewer than {} ticks inside this dispatch's {:.1} ms grace \
             window, so grace could not be delivered and every budget kill here would be \
             a hard kill, silently",
            engine.cfg.epoch_tick_us,
            crate::budget::MIN_GRACE_TICKS,
            budget.grace_ns() as f64 / 1e6,
        );

        // The watchdog only ticks while something is Hot, so this must precede
        // arming the deadline or the deadline would never be reached.
        engine.watchdog.shared().enter_hot();

        let wall0 = Instant::now();
        // Keep a failed start reading distinct from a zero-valued clock.
        let cpu0 = try_thread_cpu_nanos();
        live.store.data_mut().begin_slice(
            clock_ms,
            // The budget path keeps today's behaviour on an unreadable clock
            // (`cpu0_ns: 0`, so `dispatch_cpu_ns` reads high and the dispatch
            // is killed at its deadline rather than escaping its budget). That
            // is the conservative direction, and the counter now says it
            // happened; accounting is what this change makes honest.
            Meter { cpu0_ns: cpu0.unwrap_or(0), wall0: Some(wall0), ..meter },
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
                (PollStatus::Trapped { code: trap_code_of_kind(info.kind) }, Some(info))
            }
        };

        let violation = live.store.data().violation;
        let grace_at_ns = live.store.data().grace_at_ns;
        let grace_deferrals = live.store.data().grace_deferrals;

        // The two phases, read off the store rather than guessed:
        //
        // * a `BudgetExceeded` trap can only come from the hard deadline —
        //   there is no other producer of it — so it is a **hard** kill and
        //   the instance tombstones;
        // * a slice that entered grace and then *returned* is a **clean**
        //   kill: the guest unwound, `finally` ran, and the instance is
        //   consistent. It is still a kill (the request fails 503,
        //   `budget_exceeded`), just not a tombstone.
        //
        // A slice that entered grace and returned `pending` is neither yet:
        // it parked at an await, its poisoned future is already `Cancelled`,
        // and the wake that follows carries `spent_ns` so the rest of G is
        // what remains. Reporting it as a kill here would end a dispatch the
        // guest is still unwinding.
        let hard_kill = matches!(trap.as_ref().map(|t| t.kind), Some(TrapKind::BudgetExceeded));
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
            // Grace was entered but the dispatch has not ended yet (parked, or
            // it trapped for an unrelated reason). No phase to report.
            _ => None,
        };
        // The overage is the CPU burned past B.
        let overage_ns = if in_grace || hard_kill {
            dispatch_cpu.saturating_sub(budget.budget_ns())
        } else {
            0
        };

        if !(is_gc_hint && completion.instance_survives()) {
            self.last = Some(completion);
        }
        self.last_violation = violation;
        // The ENGINE does not poison a store after a trap, so this is where
        // the platform does.
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
            let bytes = postcard::to_allocvec(rec).map_err(wasmtime::Error::new).context("encoding dispatch record")?;
            let (ptr, len) = handoff(live, &bytes)?;
            let f = live.f_handle.clone();
            Ok(f.call(&mut live.store, (ptr, len))? as u64)
        }
        Entry::Wake(future, res) => {
            let bytes = postcard::to_allocvec(res).map_err(wasmtime::Error::new).context("encoding result record")?;
            let (ptr, len) = handoff(live, &bytes)?;
            let f = live.f_wake.clone();
            Ok(f.call(&mut live.store, (future as i64, ptr, len))? as u64)
        }
        Entry::GcHint(pressure) => {
            if let Some(f) = live.f_gc_hint.clone() {
                f.call(&mut live.store, pressure as i32)?;
            }
            // A guest that ignores the hint is correct but stores a larger
            // delta and pays for it. Not exporting it at all is the same
            // choice made earlier.
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

/// Host->guest data enters via `wisp_alloc` + copy. The guest owns the
/// allocation; the host never hands it a pointer.
fn handoff(live: &mut Live, bytes: &[u8]) -> wasmtime::Result<(i32, i32)> {
    let len = i32::try_from(bytes.len()).map_err(wasmtime::Error::new).context("record too large for the guest ABI")?;
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

/// Turn whatever ended the dispatch into a [`TrapKind`].
///
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
        return TrapInfo { kind: TrapKind::CapabilityViolation, message: v.to_string() };
    }
    if let Some(a) = err.downcast_ref::<GuestAbort>() {
        return TrapInfo { kind: TrapKind::GuestPanic, message: a.to_string() };
    }
    if let Some(a) = err.downcast_ref::<AllocFailed>() {
        return TrapInfo { kind: TrapKind::OutOfMemory, message: a.to_string() };
    }
    if let Some(r) = err.downcast_ref::<Reentrancy>() {
        return TrapInfo { kind: TrapKind::Other, message: r.to_string() };
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
        return TrapInfo { kind, message: format!("{err}") };
    }

    TrapInfo { kind: TrapKind::Other, message: format!("{err:#}") }
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
        // A budget kill has its own tenant-visible code; at the ABI level
        // it is simply a fault the guest never got to observe.
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

    /// Reset. Dropping the store releases the pooling slot, which is where
    /// the `madvise(MADV_DONTNEED)` happens; re-instantiating re-maps the
    /// template image copy-on-write and re-seals globals and tables from it.
    /// The engine does not memset anything.
    fn reset(&mut self) -> Result<()> {
        if let Some(why) = self.tombstone {
            bail!(
                "refusing to reset a TOMBSTONED instance ({why}). The slot must be reset \
                 before reuse, and for a tombstone that means dropping the whole \
                 activation and mapping the template again — not re-sealing globals over memory \
                 a trap left mid-write. Drop it and activate a fresh one."
            );
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

    /// Eligible only when Parked: empty stack, **checked, not assumed**.
    ///
    /// The check has three parts, and none of them is an assumption about what
    /// the caller did:
    ///
    /// 1. wasmtime's own call hook says no wasm frame is on the native stack;
    /// 2. the last dispatch ended by *returning* `pending`, which under
    ///    invariant 3 is the only way a guest can wait;
    /// 3. the instance is not tombstoned.
    ///
    /// (1) is why smuggled blocking never corrupts a snapshot: a
    /// guest that busy-waits never returns, so its frame never leaves the stack
    /// and it is never a capture candidate — it just dies at its CPU budget.
    fn is_parked(&self) -> bool {
        let Some(live) = self.live.as_ref() else { return false };
        self.tombstone.is_none()
            && live.store.data().wasm_depth == 0
            && self.last == Some(Completion::Parked)
    }

    /// The capture-eligibility check. The stack depth is read, not inferred — a
    /// completion enum says what the *last call returned*, and "checked, not
    /// assumed" is about the thing that would actually corrupt a delta.
    fn is_capture_eligible(&self) -> bool {
        let Some(live) = self.live.as_ref() else { return false };
        // Deltas are never captured from a tombstone. The tombstone check is
        // first because it is the one that must never be argued with.
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
            bail!(
                "refusing to capture a TOMBSTONED instance ({why}): deltas are never \
                 captured from a tombstone. Its memory is arbitrary and no cleanup ran, so \
                 storing it would persist a state no execution ever produced."
            );
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
            let baseline = template.table_nullness.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            let size = t.size(&live.store);
            if size as usize != baseline.len() {
                bail!(
                    "table `{}` grew from {} to {size} entries; the engine cannot name a funcref \
                     by function index through wasmtime's public API, so this mutation cannot be \
                     represented in a delta (documented gap)",
                    template.tables.get(i).map(|(n, _)| n.as_str()).unwrap_or("?"),
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
                        template.tables.get(i).map(|(n, _)| n.as_str()).unwrap_or("?")
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

        // Wake order: grow to the captured size first, so the delta pages
        // `wisp-hibernate` writes through `memory()` have somewhere to land.
        let cur = live.memory.size(&live.store) as u32;
        if img.memory_pages > cur {
            live.memory
                .grow(&mut live.store, (img.memory_pages - cur) as u64)
                .map_err(anyhow::Error::from)
                .context("growing linear memory to the captured size")?;
        }

        if img.globals.len() != live.globals.len() {
            bail!(
                "delta carries {} globals but this template has {} — a delta is only valid \
                 against (module hash, engine ABI rev)",
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

fn to_global_value(v: Val, store: impl wasmtime::AsContext) -> Result<crate::snapshot::GlobalValue> {
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
