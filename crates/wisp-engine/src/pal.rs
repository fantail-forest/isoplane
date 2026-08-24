//! Implements admitted WASI imports through the host capability interface.

use wisp_abi::imports;
use wisp_abi::pal::{
    lookup, Disposition, PalDef, CLI_ENV, CLOCK_QUANTUM_NS, MAX_ENTROPY_BYTES, MONOTONIC, RANDOM,
};

use crate::bindings::ImportCall;
use crate::state::StoreState;

/// True for any core-wasm module name that is not `wisp`.
///
/// The engine's own linker defines exactly `wisp`; everything else a module
/// declares has to come through the whitelist or not at all.
pub fn is_foreign_module(name: &str) -> bool {
    name != imports::HOST_MODULE
}

/// The engine's store, seen as a PAL source.
///
/// Both halves come from where the engine already keeps them rather than from
/// anywhere new, and that is what makes the no-new-channels rule hold here
/// rather than merely being restated:
///
/// * the clock is `StoreState::clock_ms` — the same field `clock_now_coarse`
///   returns, sampled once per slice and frozen (§8.5). There is no second
///   time source in this struct for a WASI door to reach.
/// * the entropy is the tenant's own `entropy_fill` binding, called through
///   the same `call_fill` path §5.5 routes every host→guest byte through. It
///   draws per call because that binding does; nothing here caches.
impl PalSource for StoreState {
    fn slice_clock_ms(&mut self) -> u64 {
        self.clock_ms
    }

    fn entropy_fill(&mut self, out: &mut [u8]) {
        // The same borrow dance an import call does: the vtable lives in this
        // struct and its methods take `&mut self`.
        let Some(mut b) = self.bindings.take() else {
            // Unreachable: the vtable is absent only *during* an import call,
            // and this IS one. Zero-filling would substitute a constant for
            // a per-call host draw, so fail loudly instead.
            panic!(
                "PAL entropy delegation reached with no binding vtable: the host would have had \
                 to zero-fill, which is a constant masquerading as a CSPRNG (§8.5)"
            );
        };
        let n = b.call_fill(
            ImportCall { import: "entropy_fill", payload: &[], scalars: [0; 3] },
            out,
        );
        self.bindings = Some(b);
        assert!(
            n >= 0 && n as usize == out.len(),
            "the host denied or short-filled a PAL entropy draw ({n} of {} bytes); a partial \
             fill leaves guest-visible zeroes that look like entropy (§8.5)",
            out.len()
        );
    }
}

/// The two `wisp:platform` capabilities the adapter is built from, and
/// nothing else.
///
/// The trait is deliberately this small: it is the machine-checked half of
/// the no-new-channels rule. An adapter body cannot reach a capability that
/// is not on this interface, so "no WASI import may introduce a capability
/// absent from `wisp:platform`" is enforced by the type system before any
/// test runs. Widening this trait is the design conversation.
pub trait PalSource: Send + 'static {
    /// §8.4/§8.5: the coarse (1 ms), intra-slice-**frozen** clock that backs
    /// `clock_now_coarse`. Frozen means: within one dispatch slice this
    /// returns the same value however many times it is called.
    fn slice_clock_ms(&mut self) -> u64;
    /// §8.5: fill `out` from the host CSPRNG. Called once per guest draw and
    /// never cached, which is what `entropy_fill` promises.
    fn entropy_fill(&mut self, out: &mut [u8]);
}

/// Compose the adapter over `module`'s `wasi:*` imports (§4.3, deploy time).
///
/// Every non-`wisp` import is looked up in [`PAL_IMPORTS`]; delegated entries
/// get a delegating body, `TrapOnCall` entries get a stub that traps. An
/// import outside the table is an error here as well as in the validator —
/// defense in depth, in the same spirit as §8.2.2's "same rule, two places,
/// one definition".
pub fn link<T: PalSource>(
    linker: &mut wasmtime::Linker<T>,
    module: &wasmtime::Module,
) -> anyhow::Result<Vec<&'static PalDef>> {
    use anyhow::bail;

    let mut composed: Vec<&'static PalDef> = Vec::new();
    for import in module.imports() {
        if import.module() == imports::HOST_MODULE {
            continue;
        }
        // A module may legally declare the same import twice; a `Linker` may
        // not be told about it twice. Real toolchain output does not do this,
        // which is exactly why a hostile module might.
        if composed.iter().any(|d| d.module == import.module() && d.func == import.name()) {
            continue;
        }
        let Some(def) = lookup(import.module(), import.name()) else {
            bail!(
                "`{}`::`{}` is not in the WASI PAL whitelist (§4.3) — the \
                 validator should have rejected this module under V1",
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
    match (def.module, def.func) {
        // §8.4/§8.5. The coarse clock, widened to nanoseconds. Multiplying is
        // the whole implementation and that is the point: the result is
        // always a whole number of milliseconds, so no sub-millisecond bit
        // exists to leak, and because `slice_clock_ms` is frozen for the
        // slice, two calls inside one dispatch return the same instant.
        (MONOTONIC, "now") => {
            linker.func_wrap(def.module, def.func, |mut c: wasmtime::Caller<'_, T>| -> i64 {
                let ms = c.data_mut().slice_clock_ms();
                // Saturating rather than wrapping: a clock that runs backwards
                // is a worse answer than one that stops, and §8.5 requires
                // monotonicity. (The saturation point is year ~294,000.)
                i64::try_from(ms).unwrap_or(i64::MAX).saturating_mul(CLOCK_QUANTUM_NS)
            })?;
        }
        (MONOTONIC, "resolution") => {
            linker.func_wrap(def.module, def.func, |_: wasmtime::Caller<'_, T>| -> i64 {
                CLOCK_QUANTUM_NS
            })?;
        }
        // §2.5.4. `list<tuple<string, string>>` lowers to a (ptr, len) pair in
        // a caller-supplied return area; (0, 0) is the empty list and needs no
        // allocation.
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
        // §8.5. `get-random-bytes(len: u64) -> list<u8>`: the canonical ABI
        // makes the CALLEE allocate, so the bytes land in guest memory via the
        // guest's own `cabi_realloc` and no host pointer ever crosses the
        // boundary (§5.5). The draw happens here, per call, from the source —
        // there is nowhere in this body for a cached buffer to live.
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
                    c.data_mut().entropy_fill(&mut bytes);
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
        _ => anyhow::bail!(
            "`{}` is marked Delegate in PAL_IMPORTS but the adapter has no body for it",
            def.full_name()
        ),
    }
    Ok(())
}

/// The stub for everything else: the import links, and calling it traps.
///
/// Generated from the module's own declared type rather than from a recorded
/// signature — see [`PalDef::sig`]. The message names the import and the
/// reason, because "never reached" is a measurement about one toolchain
/// build, not a guarantee (`docs/dotnet-findings.md` §1.3).
fn define_trap<T: PalSource>(
    linker: &mut wasmtime::Linker<T>,
    def: &'static PalDef,
    ty: wasmtime::FuncType,
) -> anyhow::Result<()> {
    linker.func_new(def.module, def.func, ty, move |_caller, _params, _results| {
        Err(wasmtime::Error::msg(format!(
            "guest called `{}`, which the WISP PAL adapter satisfies with a trap-on-call \
             stub: {} (§4.3)",
            def.full_name(),
            def.why
        )))
    })?;
    Ok(())
}

