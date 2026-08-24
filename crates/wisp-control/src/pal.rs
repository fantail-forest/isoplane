//! Admitted WASI imports and their host capability dispositions.

pub use wisp_abi::pal::*;
use wisp_abi::imports;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for p in PAL_IMPORTS {
            assert!(seen.insert((p.module, p.func)), "duplicate PAL entry {}", p.full_name());
        }
    }

    #[test]
    fn every_entry_is_a_wasi_name() {
        for p in PAL_IMPORTS {
            assert!(is_wasi(p.module), "{} is not a wasi: module", p.full_name());
            assert_ne!(p.module, imports::HOST_MODULE);
        }
    }

    /// **No-new-channels, structural half.** You cannot delegate to a
    /// capability the platform does not have, so every delegation target must
    /// be a real entry in the one capability table.
    #[test]
    fn every_delegation_targets_a_real_platform_capability() {
        for p in delegated() {
            if let Some(cap) = p.disposition.capability() {
                assert!(
                    imports::lookup(cap).is_some(),
                    "{} delegates to `{cap}`, which is not in wisp_abi::imports::IMPORTS",
                    p.full_name()
                );
            }
        }
    }

    /// **No-new-channels, the part that would hurt.** A WASI import must never
    /// become a door to egress, storage, secrets or sockets. Those groups are
    /// capabilities a tenant declares and a manifest displays; reaching one
    /// through `wasi:*` would be precisely the "capability absent from
    /// `wisp:platform`" the rule forbids — and worse, it would arrive
    /// unlabelled.
    #[test]
    fn no_delegation_reaches_an_io_or_secret_capability() {
        use wisp_abi::imports::Group::*;
        let forbidden =
            [Http, Kv, Blob, Queue, Sql, Durable, Socket, Secret, Service, Response, Ctx];
        for p in delegated() {
            let Some(cap) = p.disposition.capability() else { continue };
            let def = imports::lookup(cap).expect("checked above");
            assert!(
                !forbidden.contains(&def.group),
                "{} delegates to `{cap}` in group {:?} — a WASI import must not be a door \
                 to I/O or secrets (§4.3 no-new-channels)",
                p.full_name(),
                def.group
            );
            assert!(
                !def.sensitive,
                "{} delegates to `{cap}`, which is marked sensitive (§8.6)",
                p.full_name()
            );
        }
    }

    /// The adapter draws on exactly two `wisp:platform` capabilities. A third
    /// is a new information channel into the sandbox and must not arrive as a
    /// patch — the same argument `wisp_abi::imports`' own
    /// `host_writes_into_guest_memory_through_exactly_three_imports` makes.
    #[test]
    fn the_adapter_draws_on_exactly_two_platform_capabilities() {
        assert_eq!(delegation_targets(), vec!["clock_now_coarse", "entropy_fill"]);
    }

    /// The groups §2.5.4/§8.4 deny outright must be trap-on-call to the last
    /// entry. A single delegated filesystem or socket import would be an
    /// ambient-authority regression that no other test would notice.
    #[test]
    fn the_denied_interfaces_are_trap_on_call_without_exception() {
        for p in PAL_IMPORTS {
            let denied = p.module.starts_with("wasi:filesystem/")
                || p.module.starts_with("wasi:sockets/")
                || p.module == IO_STREAMS
                || p.module == IO_POLL
                || p.module == CLI_STDIN
                || p.module == CLI_STDOUT
                || p.module == CLI_STDERR
                || p.module == WALL_CLOCK;
            if denied {
                assert_eq!(
                    p.disposition,
                    Disposition::TrapOnCall,
                    "{} must be trap-on-call",
                    p.full_name()
                );
            }
        }
    }

    /// Delegated entries carry a checked signature; trap entries deliberately
    /// do not. Getting this backwards would either skip a type check on a body
    /// we actually run, or pin a shape on a stub we never implement.
    #[test]
    fn signatures_are_declared_exactly_for_delegated_entries() {
        for p in PAL_IMPORTS {
            assert_eq!(
                p.sig.is_some(),
                p.disposition.is_delegated(),
                "{}: sig presence must match delegation",
                p.full_name()
            );
        }
    }

    /// Every entry earns its place with a sentence. An entry with no reason is
    /// an entry nobody can audit.
    #[test]
    fn every_entry_states_why() {
        for p in PAL_IMPORTS {
            assert!(p.why.len() > 20, "{} has no usable rationale", p.full_name());
        }
    }
}
