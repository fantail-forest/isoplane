//! Guest-call mechanics: the ABI handoff into `isoplane_handle`/`isoplane_wake`, and
//! classifying what a trapped call or `Result` means.

use wasmtime::error::Context as WtContext;

use isoplane_abi::status::{PollStatus, TrapCode};

use crate::budget::TrapKind;
use crate::instance::{Entry, Live};
use crate::state::{AllocFailed, BudgetExceeded, GuestAbort, Kill, Reentrancy, WallCapExceeded};
use crate::TrapInfo;

pub(crate) fn call_entry(live: &mut Live, entry: Entry<'_>) -> wasmtime::Result<u64> {
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
        Entry::Initialize => match live.f_initialize.clone() {
            Some(f) => Ok(f.call(&mut live.store, ())? as u64),
            None => Ok(PollStatus::Done { response: 0 }.encode()),
        },
        Entry::InitializeRuntime => match live.f_initialize_runtime.clone() {
            Some(f) => Ok(f.call(&mut live.store, ())? as u64),
            None => Ok(PollStatus::Done { response: 0 }.encode()),
        },
    }
}

/// Host-to-guest data enters via `isoplane_alloc` plus a copy; the guest owns the
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
        "isoplane_alloc pointer overflow",
    )?;
    WtContext::context(
        data.get_mut(start..end),
        "isoplane_alloc returned a pointer outside linear memory",
    )?
    .copy_from_slice(bytes);
    Ok((ptr, len))
}

/// The engine's own reasons are recorded in the store rather than recovered
/// from the error chain, so budget kills and capability violations are
/// classified exactly instead of by matching wasmtime's message strings.
pub(crate) fn classify(live: &Live, err: &wasmtime::Error) -> TrapInfo {
    let d = live.store.data();

    if let Some(kill) = d.kill {
        let kind = TrapKind::BudgetExceeded;
        let message = match kill {
            Kill::OccupancyHard => err
                .downcast_ref::<BudgetExceeded>()
                .map(|b| b.to_string())
                .unwrap_or_else(|| "occupancy budget exceeded".into()),
            Kill::Wall => err
                .downcast_ref::<WallCapExceeded>()
                .map(|w| format!("{w} (per-invocation elapsed-time cap)"))
                .unwrap_or_else(|| "wall-clock backstop tripped".into()),
        };
        return TrapInfo { kind, message };
    }

    if err
        .downcast_ref::<crate::meter::OccupancyClockUnavailable>()
        .is_some()
    {
        return TrapInfo {
            kind: TrapKind::OccupancyClockUnavailable,
            message: "host occupancy clock unavailable".into(),
        };
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
            T::MappedStorageFault => TrapKind::StorageFault,
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

pub(crate) fn trap_kind_of_code(c: TrapCode) -> TrapKind {
    match c {
        TrapCode::Panic => TrapKind::GuestPanic,
        TrapCode::OutOfMemory => TrapKind::OutOfMemory,
        TrapCode::CapabilityViolation => TrapKind::CapabilityViolation,
        TrapCode::Protocol | TrapCode::TooManyFutures | TrapCode::Unknown => TrapKind::Other,
    }
}

pub(crate) fn trap_code_of_kind(k: TrapKind) -> TrapCode {
    match k {
        TrapKind::GuestPanic => TrapCode::Panic,
        TrapKind::OutOfMemory => TrapCode::OutOfMemory,
        TrapKind::CapabilityViolation => TrapCode::CapabilityViolation,
        TrapKind::BudgetExceeded
        | TrapKind::Discarded
        | TrapKind::Sandbox
        | TrapKind::OccupancyClockUnavailable
        | TrapKind::StorageFault
        | TrapKind::Other => TrapCode::Unknown,
    }
}
