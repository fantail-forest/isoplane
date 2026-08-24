//! The import vtable (spec.md §5.3, §5.5, invariant 4).
//!
//! > *The guest can affect the world exclusively through typed imports declared
//! > in its component's WIT world.*
//!
//! The linker defines **exactly** [`wisp_abi::imports::IMPORTS`] under module
//! `wisp` and nothing else, so a module importing anything else fails
//! instantiation. That is rule V1 (§4.3) enforced a second time: the validator
//! rejects the module at deploy time, and if a module somehow reached a cell
//! anyway there is no symbol for it to bind to. There is deliberately no
//! `define_unknown_imports_as_traps` here — an undeclared import must be a
//! *deploy* failure, not a runtime one.
//!
//! Two rules govern every buffer that crosses the boundary (§5.5):
//!
//! * guest → host: the engine copies the bytes **out** of linear memory before
//!   calling the binding, so the host never holds a pointer into a memory the
//!   guest could grow underneath it;
//! * host → guest: the binding fills a host-side scratch buffer and the engine
//!   copies it **in**.
//!
//! In neither direction does a host pointer enter guest memory.

use wasmtime::error::Context;
use wasmtime::{bail, Caller, Engine, Linker, Result};
use wisp_abi::imports::{ImportDef, ImportKind, Ty, HOST_MODULE, IMPORTS};

use crate::bindings::ImportCall;
use crate::layout;
use crate::state::{CapabilityViolation, GuestAbort, StoreState};

pub(crate) fn build_linker(engine: &Engine) -> Result<Linker<StoreState>> {
    let mut l: Linker<StoreState> = Linker::new(engine);
    for def in IMPORTS {
        define(&mut l, def).with_context(|| format!("defining import `{}`", def.name))?;
    }
    Ok(l)
}

/// Register one import under its declared core-wasm signature.
///
/// The shapes are enumerated rather than generated because the set is small,
/// closed, and normative: a new shape appearing in `IMPORTS` should be a
/// compile-visible decision here, not something the engine silently guesses.
fn define(l: &mut Linker<StoreState>, def: &'static ImportDef) -> Result<()> {
    use Ty::{I32, I64};
    let m = HOST_MODULE;
    let n = def.name;

    macro_rules! wrap {
        (|$c:ident $(, $a:ident : $t:ty)*| -> $ret:ty { $body:expr }) => {
            l.func_wrap(m, n, move |mut $c: Caller<'_, StoreState>, $($a: $t),*| -> Result<$ret> {
                $body
            })?
        };
    }

    match (def.sig.params, def.sig.results) {
        ([], [I64]) => wrap!(|c| -> i64 { Ok(invoke(&mut c, def, &[])? as i64) }),
        ([I32], []) => wrap!(|c, a: i32| -> () {
            invoke(&mut c, def, &[a as u32 as u64]).map(|_| ())
        }),
        ([I32], [I32]) => wrap!(|c, a: i32| -> i32 {
            Ok(invoke(&mut c, def, &[a as u32 as u64])? as u32 as i32)
        }),
        ([I64], []) => wrap!(|c, a: i64| -> () {
            invoke(&mut c, def, &[a as u64]).map(|_| ())
        }),
        ([I64], [I64]) => wrap!(|c, a: i64| -> i64 {
            Ok(invoke(&mut c, def, &[a as u64])? as i64)
        }),
        ([I32, I32], []) => wrap!(|c, a: i32, b: i32| -> () {
            invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64]).map(|_| ())
        }),
        ([I32, I32], [I32]) => wrap!(|c, a: i32, b: i32| -> i32 {
            Ok(invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64])? as u32 as i32)
        }),
        ([I32, I32], [I64]) => wrap!(|c, a: i32, b: i32| -> i64 {
            Ok(invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64])? as i64)
        }),
        ([I32, I32, I32], []) => wrap!(|c, a: i32, b: i32, d: i32| -> () {
            invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64, d as u32 as u64]).map(|_| ())
        }),
        ([I32, I32, I32], [I64]) => wrap!(|c, a: i32, b: i32, d: i32| -> i64 {
            Ok(invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64, d as u32 as u64])? as i64)
        }),
        ([I32, I32, I64], []) => wrap!(|c, a: i32, b: i32, d: i64| -> () {
            invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64, d as u64]).map(|_| ())
        }),
        (p, r) => bail!("no lowering for signature {p:?} -> {r:?}"),
    };
    Ok(())
}

/// The body shared by every import.
fn invoke(c: &mut Caller<'_, StoreState>, def: &'static ImportDef, raw: &[u64]) -> Result<u64> {
    // §4.4: the sealed init sandbox, where "every I/O import traps". Checked
    // first, so a forbidden import cannot have a side effect on its way to
    // trapping — that is what makes pre-init's rollback-by-replay a genuine
    // rollback. The offending name is recorded because §4.4's deferral loop
    // needs to know *which* import an impure static initializer touched.
    if c.data().is_init_sandbox() && !def.init_visible {
        c.data_mut().violation = Some(def.name);
        return Err(CapabilityViolation { import: def.name, init_sandbox: true }.into());
    }

    match def.name {
        // §8.4/§8.5. The frozen clock never reaches a binding: it is an
        // engine-owned security control. Returning a host-sourced value here
        // would hand the guest back the measurement primitive the freeze exists
        // to remove.
        "clock_now_coarse" => return Ok(c.data().clock_ms),
        // §5.3 core group. An abort is a guest fault, so it leaves as a trap
        // rather than a return value; the message is kept for telemetry.
        "abort" => {
            let bytes = read_guest(c, raw[0] as usize, raw[1] as usize).unwrap_or_default();
            let message = String::from_utf8_lossy(&bytes).into_owned();
            c.data_mut().abort_msg = Some(message.clone());
            return Err(GuestAbort { message }.into());
        }
        _ => {}
    }

    let scalars = layout::scalars_of(def, raw);

    // Borrow dance: `HostBindings` methods take `&mut self`, and both the
    // bindings and the scratch buffer live in the store data, so both are moved
    // out for the duration of the call.
    let mut scratch = std::mem::take(&mut c.data_mut().scratch);
    scratch.clear();

    let result = call_binding(c, def, raw, scalars, &mut scratch);

    scratch.clear();
    c.data_mut().scratch = scratch;
    result
}

fn call_binding(
    c: &mut Caller<'_, StoreState>,
    def: &'static ImportDef,
    raw: &[u64],
    scalars: [u64; 3],
    scratch: &mut Vec<u8>,
) -> Result<u64> {
    use wisp_abi::imports::BufSpec;

    // Guest -> host: copy out before the call (§5.5).
    if let BufSpec::In(i) = def.buf {
        let i = i as usize;
        let (ptr, len) = (raw[i] as usize, raw[i + 1] as usize);
        let bytes = read_guest(c, ptr, len)?;
        *scratch = bytes;
    }
    // Host -> guest: the second parameter is a *capacity*, not a length. The
    // host may report a larger value than it could fit (`init_config_get`
    // answers a zero-length probe with the blob's full size), so the engine
    // never treats the return value as a byte count to copy.
    if let BufSpec::Out(i) = def.buf {
        let cap = raw[i as usize + 1] as usize;
        if cap > MAX_OUT_BUFFER {
            bail!("`{}` out-buffer of {cap} bytes exceeds the {MAX_OUT_BUFFER} byte cap", def.name);
        }
        scratch.clear();
        scratch.resize(cap, 0);
    }

    let mut bindings = c
        .data_mut()
        .bindings
        .take()
        .context("import called reentrantly: the binding vtable is already in use (§5.4)")?;

    let call = ImportCall { import: def.name, payload: scratch.as_slice(), scalars };
    let ret = if layout::is_out(def) {
        // The one case where bytes flow host -> guest inside an import. The
        // host writes into a host-side buffer; the engine copies it in below.
        bindings.call_fill(
            ImportCall { import: def.name, payload: &[], scalars },
            scratch.as_mut_slice(),
        ) as u64
    } else {
        match def.kind {
            // §5.1: "there is no import that waits". A future-returning import
            // MUST return immediately with a future id, which is what makes
            // invariant 3 hold by construction rather than by discipline.
            ImportKind::Future => bindings.call_future(call),
            ImportKind::Sync => bindings.call_sync(call) as u64,
        }
    };

    // §8.5: the coarse clock "advances only at host-call/wake boundaries". A
    // future-returning import is that boundary: control genuinely left the
    // guest. Synchronous imports do *not* advance it, so no amount of
    // `log_emit` in a loop turns the clock into a stopwatch (§8.4). Monotonic
    // per instance, as §8.5 requires.
    if matches!(def.kind, ImportKind::Future) {
        let now = bindings.slice_clock_ms();
        let d = c.data_mut();
        d.clock_ms = d.clock_ms.max(now);
    }

    c.data_mut().bindings = Some(bindings);

    // Host -> guest: copy in after the call (§5.5).
    if let BufSpec::Out(i) = def.buf {
        let ptr = raw[i as usize] as usize;
        write_guest(c, ptr, scratch.as_slice())?;
    }

    Ok(ret)
}

/// Ceiling on a single host->guest fill, so a guest cannot ask the host to
/// allocate an arbitrary buffer on its behalf (§11.1 is about the guest's own
/// memory; this is about the host's).
const MAX_OUT_BUFFER: usize = 1 << 20;

pub(crate) fn read_guest(c: &mut Caller<'_, StoreState>, ptr: usize, len: usize) -> Result<Vec<u8>> {
    let mem = c.data().memory.context("instance has no `memory` export")?;
    let end = ptr.checked_add(len).context("import buffer address overflow")?;
    let data = mem.data(&*c);
    let slice = data
        .get(ptr..end)
        .with_context(|| format!("import buffer [{ptr}, {end}) is outside a {} byte linear memory", data.len()))?;
    Ok(slice.to_vec())
}

fn write_guest(c: &mut Caller<'_, StoreState>, ptr: usize, bytes: &[u8]) -> Result<()> {
    let mem = c.data().memory.context("instance has no `memory` export")?;
    let end = ptr.checked_add(bytes.len()).context("import buffer address overflow")?;
    let data = mem.data_mut(&mut *c);
    let dst = data
        .get_mut(ptr..end)
        .with_context(|| format!("out-buffer [{ptr}, {end}) is outside linear memory"))?;
    dst.copy_from_slice(bytes);
    Ok(())
}
