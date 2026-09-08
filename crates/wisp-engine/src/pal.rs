//! The WASI PAL adapter's bodies. The table is [`wisp_abi::pal::PAL_IMPORTS`];
//! the bodies live in the engine because the crate that instantiates a module
//! must satisfy a `wasi:*` import exactly as the validator admitted it.
//!
//! Grace poisoning and the adapter both intercept imports. The delegated
//! sources, `clock_now_coarse` and `entropy_fill`, and the empty filesystem
//! adapter are synchronous, and
//! poisoning applies only to future-returning imports, so a delegated entry
//! keeps answering during grace; a trap stub keeps trapping, since a grace
//! window does not turn a denied capability on.

pub mod filesystem;
pub mod native_io;

use anyhow::bail;
use wasmtime::bail as wt_bail;
use wisp_abi::imports;
use wisp_abi::pal::{
    lookup, Disposition, PalDef, CLI_ENV, CLOCK_QUANTUM_NS, MAX_ENTROPY_BYTES, MONOTONIC, RANDOM,
};

use crate::bindings::ImportCall;
use crate::state::StoreState;

/// Any core-wasm module name other than `wisp`.
pub(crate) fn is_foreign_module(name: &str) -> bool {
    name != imports::HOST_MODULE
}

/// Both halves come from where the engine already keeps them: the clock is
/// the field `clock_now_coarse` returns, and the entropy is the tenant's own
/// `entropy_fill` binding through the same `call_fill` path every host-to-guest
/// byte takes.
impl PalSource for StoreState {
    fn native_io(
        &mut self,
        request: wisp_abi::native_io::Request,
    ) -> Result<wisp_abi::native_io::Response, wisp_abi::native_io::Error> {
        if self.is_init_sandbox() {
            return Err(wisp_abi::native_io::Error::Denied(
                "native I/O during initialization".into(),
            ));
        }
        self.bindings
            .as_mut()
            .ok_or(wisp_abi::native_io::Error::Unsupported(
                "native I/O without host bindings",
            ))?
            .native_io(request)
    }

    fn slice_clock_ms(&mut self) -> u64 {
        self.clock_ms
    }

    fn entropy_fill(&mut self, out: &mut [u8]) -> wasmtime::Result<()> {
        let Some(mut b) = self.bindings.take() else {
            wt_bail!("PAL entropy delegation reached while the binding vtable was in use");
        };
        let n = b.call_fill(
            ImportCall {
                import: "entropy_fill",
                payload: &[],
                scalars: [0; 3],
            },
            out,
        );
        self.bindings = Some(b);
        if n < 0 || n as usize != out.len() {
            // A partial fill leaves guest-visible zeroes that look like entropy.
            wt_bail!(
                "the host denied or short-filled a PAL entropy draw ({n} of {} bytes)",
                out.len()
            );
        }
        Ok(())
    }
}

/// Host capabilities available to composed native runtime imports.
/// Sealed initialization provides clock and entropy but refuses readiness resources.
pub trait PalSource: Send + 'static {
    /// Perform nonblocking native I/O; sealed initialization has no resource authority.
    fn native_io(
        &mut self,
        _request: wisp_abi::native_io::Request,
    ) -> Result<wisp_abi::native_io::Response, wisp_abi::native_io::Error> {
        Err(wisp_abi::native_io::Error::Denied(
            "native I/O unavailable in this execution context".into(),
        ))
    }
    /// The coarse clock, frozen for the slice.
    fn slice_clock_ms(&mut self) -> u64;
    /// Fill `out` from the host CSPRNG, once per guest draw.
    fn entropy_fill(&mut self, out: &mut [u8]) -> wasmtime::Result<()>;
}

/// Compose the adapter over `module`'s `wasi:*` imports. An import outside
/// the table is an error here as well as in the validator.
pub fn link<T: PalSource>(
    linker: &mut wasmtime::Linker<T>,
    module: &wasmtime::Module,
) -> anyhow::Result<Vec<&'static PalDef>> {
    let mut composed: Vec<&'static PalDef> = Vec::new();
    for import in module.imports() {
        if import.module() == imports::HOST_MODULE {
            continue;
        }
        // A module may declare the same import twice; a `Linker` may not be
        // told about it twice.
        if composed
            .iter()
            .any(|d| d.module == import.module() && d.func == import.name())
        {
            continue;
        }
        let Some(def) = lookup(import.module(), import.name()) else {
            bail!(
                "`{}`::`{}` is not in the WASI PAL whitelist",
                import.module(),
                import.name()
            );
        };
        let wasmtime::ExternType::Func(ty) = import.ty() else {
            bail!("`{}` is not a function import", def.full_name());
        };
        match def.disposition {
            Disposition::Delegate(_) => define_delegate(linker, def)?,
            Disposition::TrapOnCall => define_trap(linker, def, ty)?,
        }
        composed.push(def);
    }
    Ok(composed)
}

fn memory_of<T>(caller: &mut wasmtime::Caller<'_, T>) -> wasmtime::Result<wasmtime::Memory> {
    match caller.get_export("memory") {
        Some(wasmtime::Extern::Memory(m)) => Ok(m),
        _ => Err(wasmtime::Error::msg("instance has no `memory` export")),
    }
}

fn define_delegate<T: PalSource>(
    linker: &mut wasmtime::Linker<T>,
    def: &'static PalDef,
) -> anyhow::Result<()> {
    if let Some(native) = wisp_abi::native_io::canonical::DEFINITIONS
        .iter()
        .find(|native| native.pal.module == def.module && native.pal.func == def.func)
    {
        let signature = native
            .pal
            .sig
            .ok_or_else(|| anyhow::anyhow!("native signature missing"))?;
        let value_type = |ty: &wisp_abi::imports::Ty| match ty {
            wisp_abi::imports::Ty::I32 => wasmtime::ValType::I32,
            wisp_abi::imports::Ty::I64 => wasmtime::ValType::I64,
        };
        let ty = wasmtime::FuncType::new(
            linker.engine(),
            signature.params.iter().map(value_type),
            signature.results.iter().map(value_type),
        );
        linker.func_new(
            def.module,
            def.func,
            ty,
            move |mut caller, args, results| {
                let mut scalars = [0u64; wisp_abi::native_io::canonical::MAX_ARGUMENTS];
                for (slot, value) in scalars.iter_mut().zip(args) {
                    *slot = match value {
                        wasmtime::Val::I32(value) => *value as u32 as u64,
                        wasmtime::Val::I64(value) => *value as u64,
                        _ => return Err(wasmtime::Error::msg("native argument is not an integer")),
                    };
                }
                let value = native_io::call(&mut caller, native, &scalars[..args.len()])
                    .map_err(wasmtime::Error::from_anyhow)?;
                if let Some(result) = results.first_mut() {
                    *result = match signature.results[0] {
                        wisp_abi::imports::Ty::I32 => wasmtime::Val::I32(value as i32),
                        wisp_abi::imports::Ty::I64 => wasmtime::Val::I64(value as i64),
                    };
                }
                Ok(())
            },
        )?;
        return Ok(());
    }
    if let Some(fs) = wisp_abi::filesystem::IMPORTS
        .iter()
        .find(|fs| fs.pal.module == def.module && fs.pal.func == def.func)
    {
        let signature = fs
            .pal
            .sig
            .ok_or_else(|| anyhow::anyhow!("filesystem signature missing"))?;
        let types = signature.params.iter().map(|ty| match ty {
            wisp_abi::imports::Ty::I32 => wasmtime::ValType::I32,
            wisp_abi::imports::Ty::I64 => wasmtime::ValType::I64,
        });
        let ty = wasmtime::FuncType::new(linker.engine(), types, []);
        linker.func_new(
            def.module,
            def.func,
            ty,
            move |mut caller, args, _results| {
                let mut scalars = [0u64; wisp_abi::filesystem::MAX_ARGUMENTS];
                for (index, arg) in args.iter().enumerate() {
                    scalars[index] = match arg {
                        wasmtime::Val::I32(value) => *value as u32 as u64,
                        wasmtime::Val::I64(value) => *value as u64,
                        _ => {
                            return Err(wasmtime::Error::msg(
                                "filesystem argument is not an integer",
                            ))
                        }
                    };
                }
                filesystem::call(
                    &mut filesystem::CallerMemory(&mut caller),
                    fs,
                    &scalars[..args.len()],
                )
                .map_err(wasmtime::Error::from_anyhow)
            },
        )?;
        return Ok(());
    }
    match (def.module, def.func) {
        // The coarse clock multiplied into nanoseconds: always a whole number
        // of milliseconds, so no sub-millisecond bit exists to leak.
        (MONOTONIC, "now") => {
            linker.func_wrap(
                def.module,
                def.func,
                |mut c: wasmtime::Caller<'_, T>| -> i64 {
                    let ms = c.data_mut().slice_clock_ms();
                    // Saturating: a clock that stops is a better answer than
                    // one that runs backwards.
                    i64::try_from(ms)
                        .unwrap_or(i64::MAX)
                        .saturating_mul(CLOCK_QUANTUM_NS)
                },
            )?;
        }
        (MONOTONIC, "resolution") => {
            linker.func_wrap(def.module, def.func, |_: wasmtime::Caller<'_, T>| -> i64 {
                CLOCK_QUANTUM_NS
            })?;
        }
        // `list<tuple<string, string>>` lowers to a (ptr, len) pair in a
        // caller-supplied return area; (0, 0) is the empty list.
        (CLI_ENV, "get-environment") => {
            linker.func_wrap(
                def.module,
                def.func,
                |mut c: wasmtime::Caller<'_, T>, retptr: i32| -> wasmtime::Result<()> {
                    let mem = memory_of(&mut c)?;
                    mem.write(&mut c, retptr as usize, &[0u8; 8])?;
                    Ok(())
                },
            )?;
        }
        // `get-random-bytes(len: u64) -> list<u8>`: the canonical ABI has the
        // callee allocate through the guest's `cabi_realloc`, so the bytes
        // land in guest memory without a host pointer crossing the boundary.
        (RANDOM, "get-random-bytes") => {
            linker.func_wrap(
                def.module,
                def.func,
                |mut c: wasmtime::Caller<'_, T>, len: i64, retptr: i32| -> wasmtime::Result<()> {
                    let n = if len < 0 { 0u64 } else { len as u64 };
                    if n > MAX_ENTROPY_BYTES {
                        return Err(wasmtime::Error::msg(format!(
                            "guest asked for {n} bytes of entropy; the per-call ceiling is \
                             {MAX_ENTROPY_BYTES}"
                        )));
                    }
                    let ptr = if n == 0 {
                        0i32
                    } else {
                        let realloc = match c.get_export("cabi_realloc") {
                            Some(wasmtime::Extern::Func(f)) => f,
                            _ => {
                                return Err(wasmtime::Error::msg(format!(
                                    "guest declares `{RANDOM}::get-random-bytes` but exports \
                                     no `cabi_realloc` for the canonical ABI to allocate \
                                     through"
                                )))
                            }
                        };
                        let realloc = realloc.typed::<(i32, i32, i32, i32), i32>(&c)?;
                        realloc.call(&mut c, (0, 0, 1, n as i32))?
                    };
                    let mut bytes = vec![0u8; n as usize];
                    c.data_mut().entropy_fill(&mut bytes)?;
                    let mem = memory_of(&mut c)?;
                    if n > 0 {
                        mem.write(&mut c, ptr as usize, &bytes)?;
                    }
                    mem.write(&mut c, retptr as usize, &(ptr as u32).to_le_bytes())?;
                    mem.write(&mut c, retptr as usize + 4, &(n as u32).to_le_bytes())?;
                    Ok(())
                },
            )?;
        }
        _ => bail!(
            "`{}` is marked Delegate in PAL_IMPORTS but the adapter has no body for it",
            def.full_name()
        ),
    }
    Ok(())
}

/// The import links, and calling it traps with a message naming the import
/// and the reason. Typed from the module's own declaration; see
/// [`PalDef::sig`].
fn define_trap<T: PalSource>(
    linker: &mut wasmtime::Linker<T>,
    def: &'static PalDef,
    ty: wasmtime::FuncType,
) -> anyhow::Result<()> {
    linker.func_new(
        def.module,
        def.func,
        ty,
        move |_caller, _params, _results| {
            Err(wasmtime::Error::msg(format!(
                "guest called `{}`, which the WISP PAL adapter satisfies with a trap-on-call \
             stub: {}",
                def.full_name(),
                def.why
            )))
        },
    )?;
    Ok(())
}
