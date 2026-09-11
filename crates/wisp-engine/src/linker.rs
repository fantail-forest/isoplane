//! The import vtable. The linker defines exactly [`wisp_abi::imports::IMPORTS`]
//! under module `wisp`, so a module importing anything else fails to
//! instantiate. Guest buffers are copied out before the binding is called and
//! host results are copied in afterwards; in neither direction does a host
//! pointer enter guest memory.

use wasmtime::error::Context;
use wasmtime::{bail, Caller, Engine, Linker, Result};
use wisp_abi::imports::{ImportDef, ImportKind, Ty, HOST_MODULE, IMPORTS, MAX_OUT_BUFFER_BYTES};

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

/// The shapes are enumerated rather than generated because the set is small
/// and closed: a new shape in `IMPORTS` is a decision here, not a guess.
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
        ([], [I32]) => wrap!(|c| -> i32 { Ok(invoke(&mut c, def, &[])? as u32 as i32) }),
        ([], [I64]) => wrap!(|c| -> i64 { Ok(invoke(&mut c, def, &[])? as i64) }),
        ([I32], []) => {
            wrap!(|c, a: i32| -> () { invoke(&mut c, def, &[a as u32 as u64]).map(|_| ()) })
        }
        ([I32], [I32]) => {
            wrap!(|c, a: i32| -> i32 { Ok(invoke(&mut c, def, &[a as u32 as u64])? as u32 as i32) })
        }
        ([I32], [I64]) => {
            wrap!(|c, a: i32| -> i64 { Ok(invoke(&mut c, def, &[a as u32 as u64])? as i64) })
        }
        ([I64], []) => wrap!(|c, a: i64| -> () { invoke(&mut c, def, &[a as u64]).map(|_| ()) }),
        ([I64], [I64]) => {
            wrap!(|c, a: i64| -> i64 { Ok(invoke(&mut c, def, &[a as u64])? as i64) })
        }
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
            invoke(
                &mut c,
                def,
                &[a as u32 as u64, b as u32 as u64, d as u32 as u64],
            )
            .map(|_| ())
        }),
        ([I32, I32, I32], [I64]) => wrap!(|c, a: i32, b: i32, d: i32| -> i64 {
            Ok(invoke(
                &mut c,
                def,
                &[a as u32 as u64, b as u32 as u64, d as u32 as u64],
            )? as i64)
        }),
        ([I32, I32, I64], []) => wrap!(|c, a: i32, b: i32, d: i64| -> () {
            invoke(&mut c, def, &[a as u32 as u64, b as u32 as u64, d as u64]).map(|_| ())
        }),
        (p, r) => bail!("no lowering for signature {p:?} -> {r:?}"),
    };
    Ok(())
}

fn invoke(c: &mut Caller<'_, StoreState>, def: &'static ImportDef, raw: &[u64]) -> Result<u64> {
    // Refuse this call before its side effect. Earlier permitted initialization
    // I/O is not undone when preparation retries a deferred unit.
    if c.data().is_init_sandbox() && !def.init_visible {
        c.data_mut().violation = Some(def.name);
        return Err(CapabilityViolation {
            import: def.name,
            init_sandbox: true,
        }
        .into());
    }

    match def.name {
        "socket_call" | "poll_call" => {
            return crate::pal::native_io::packet(
                c,
                def.name,
                raw[0] as u32,
                raw[1] as u32,
                raw[2] as u32,
            )
            .map_err(wasmtime::Error::from_anyhow);
        }
        "filesystem_call" => {
            crate::pal::filesystem::packet(
                &mut crate::pal::filesystem::CallerMemory(c),
                raw[0] as u32,
                raw[1] as u32,
                raw[2] as u32,
            )
            .map_err(wasmtime::Error::from_anyhow)?;
            return Ok(0);
        }
        // Engine-owned: a host-sourced value here would hand the guest back
        // the timing primitive the freeze removes.
        "clock_now_coarse" => return Ok(c.data().clock_ms),
        // Engine-owned: reads the meter the epoch callback compares against.
        // Not a fine clock, since it moves only with the guest's own thread
        // CPU.
        "budget_remaining_ns" => {
            let m = c.data().meter;
            let cpu = m.dispatch_cpu_ns(crate::meter::thread_cpu_nanos()?);
            let remaining = (m.cpu_budget_ns as i128) - (cpu as i128);
            return Ok(remaining.clamp(i64::MIN as i128, i64::MAX as i128) as i64 as u64);
        }
        "unsafe_to_trap" => {
            c.data_mut().unsafe_to_trap = raw[0] != 0;
            return Ok(0);
        }
        "abort" => {
            let bytes = read_guest(c, raw[0] as usize, raw[1] as usize).unwrap_or_default();
            let message = String::from_utf8_lossy(&bytes).into_owned();
            c.data_mut().abort_msg = Some(message.clone());
            return Err(GuestAbort { message }.into());
        }
        _ => {}
    }

    let scalars = layout::scalars_of(def, raw);

    // `HostBindings` methods take `&mut self`, and both the bindings and the
    // scratch buffer live in the store data, so both are moved out for the
    // call.
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

    if let BufSpec::In(i) = def.buf {
        let i = i as usize;
        let (ptr, len) = (raw[i] as usize, raw[i + 1] as usize);
        let bytes = read_guest(c, ptr, len)?;
        *scratch = bytes;
    }
    // The second parameter of an out-buffer is a capacity. The host may
    // report a larger value than fits (`init_config_get` answers a zero-length
    // probe with the blob's full size), so the return value is never treated
    // as a byte count to copy.
    if let BufSpec::Out(i) = def.buf {
        let cap = raw[i as usize + 1] as usize;
        if cap > MAX_OUT_BUFFER_BYTES {
            bail!(
                "`{}` out-buffer of {cap} bytes exceeds the {MAX_OUT_BUFFER_BYTES} byte cap",
                def.name
            );
        }
        scratch.clear();
        scratch.resize(cap, 0);
    }

    let mut bindings = c
        .data_mut()
        .bindings
        .take()
        .context("import called reentrantly: the binding vtable is already in use")?;

    let call = ImportCall {
        import: def.name,
        payload: scratch.as_slice(),
        scalars,
    };
    let ret = if layout::is_out(def) {
        bindings.call_fill(
            ImportCall {
                import: def.name,
                payload: &[],
                scalars,
            },
            scratch.as_mut_slice(),
        ) as u64
    } else {
        match def.kind {
            ImportKind::Future => bindings.call_future(call),
            ImportKind::Sync => bindings.call_sync(call) as u64,
        }
    };

    // Future-returning imports resample the binding's clock. Cell bindings
    // keep that value frozen until the next slice; synchronous imports do
    // not resample it.
    if matches!(def.kind, ImportKind::Future) {
        let now = bindings.slice_clock_ms();
        let d = c.data_mut();
        d.clock_ms = d.clock_ms.max(now);
    }

    c.data_mut().bindings = Some(bindings);

    if def.name == "entropy_fill" && ret != scratch.len() as u64 {
        bail!(
            "the host failed to fill an entropy draw (reported {} of {} bytes)",
            ret as i64,
            scratch.len()
        );
    }

    if let BufSpec::Out(i) = def.buf {
        let ptr = raw[i as usize] as usize;
        write_guest(c, ptr, scratch.as_slice())?;
    }

    Ok(ret)
}

pub(crate) fn read_guest(
    c: &mut Caller<'_, StoreState>,
    ptr: usize,
    len: usize,
) -> Result<Vec<u8>> {
    let mem = c.data().memory.context("instance has no `memory` export")?;
    let end = ptr
        .checked_add(len)
        .context("import buffer address overflow")?;
    let data = mem.data(&*c);
    let slice = data.get(ptr..end).with_context(|| {
        format!(
            "import buffer [{ptr}, {end}) is outside a {} byte linear memory",
            data.len()
        )
    })?;
    Ok(slice.to_vec())
}

fn write_guest(c: &mut Caller<'_, StoreState>, ptr: usize, bytes: &[u8]) -> Result<()> {
    let mem = c.data().memory.context("instance has no `memory` export")?;
    let end = ptr
        .checked_add(bytes.len())
        .context("import buffer address overflow")?;
    let data = mem.data_mut(&mut *c);
    let dst = data
        .get_mut(ptr..end)
        .with_context(|| format!("out-buffer [{ptr}, {end}) is outside linear memory"))?;
    dst.copy_from_slice(bytes);
    Ok(())
}
