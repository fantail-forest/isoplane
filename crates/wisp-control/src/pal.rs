//! Admitted WASI imports and their host capability dispositions.

use wisp_abi::imports::{self, Sig, Ty};

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// Every WISP import lives under core-wasm module `wisp`; every WASI import
/// under a module name beginning with this.
pub const WASI_PREFIX: &str = "wasi:";

/// What a delegated WASI import actually draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegation {
    /// The `wisp:platform` import that supplies the value. MUST name an entry
    /// in [`wisp_abi::imports::IMPORTS`] — you cannot delegate to a capability
    /// the platform does not have.
    Cap(&'static str),
    /// There is **no** `wisp:platform` capability to delegate to, and that is
    /// the answer: the adapter returns the empty value. Same fidelity as
    /// absent, which is what the no-new-channels rule asks for. Used only
    /// where the spec denies the capability outright (§2.5.4: there is no
    /// ambient environment).
    Nothing,
}

/// What the deploy-time composition does with a whitelisted `wasi:*` import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Satisfied by delegation, under the no-new-channels rule.
    Delegate(Delegation),
    /// Satisfied by a stub that **traps when called**. The declaration
    /// validates; the capability is absent. Chosen wherever a delegation
    /// could not be made channel-equivalent — a trap is honest, a leaky
    /// delegation is not.
    TrapOnCall,
}

impl Disposition {
    pub fn is_delegated(&self) -> bool {
        matches!(self, Disposition::Delegate(_))
    }
    /// The `wisp:platform` import this resolves to, if any. `None` covers both
    /// `TrapOnCall` and [`Delegation::Nothing`].
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            Disposition::Delegate(Delegation::Cap(c)) => Some(c),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PalDef {
    /// Core-wasm module name, e.g. `wasi:random/random@0.2.0`.
    pub module: &'static str,
    /// Core-wasm field name, e.g. `get-random-bytes`. Resource methods keep
    /// their canonical-ABI spelling (`[method]descriptor.stat`,
    /// `[resource-drop]pollable`).
    pub func: &'static str,
    pub disposition: Disposition,
    /// The canonical-ABI core signature, **checked only for delegated
    /// entries**.
    ///
    /// For a delegated import the shape is load-bearing: the adapter runs a
    /// body against it, and a mismatch would be a type confusion reaching
    /// host code — the same argument [`crate::validator`] makes for the
    /// `wisp` table. For a `TrapOnCall` entry it is not: the stub is
    /// generated from whatever type the module declares and traps before
    /// touching an argument, so pinning a shape we never implement would be
    /// theatre that breaks the first time a toolchain lowers the same
    /// function differently.
    pub sig: Option<Sig>,
    /// Why this disposition and not the other. Read by the auditor.
    pub why: &'static str,
}

impl PalDef {
    /// `wasi:random/random@0.2.0::get-random-bytes` — the spelling used in
    /// `docs/dotnet-findings.md`, in violation messages, and in the manifest.
    pub fn full_name(&self) -> String {
        format!("{}::{}", self.module, self.func)
    }
}

const CLI_ENV: &str = "wasi:cli/environment@0.2.0";
const CLI_EXIT: &str = "wasi:cli/exit@0.2.0";
const CLI_STDIN: &str = "wasi:cli/stdin@0.2.0";
const CLI_STDOUT: &str = "wasi:cli/stdout@0.2.0";
const CLI_STDERR: &str = "wasi:cli/stderr@0.2.0";
const CLI_TERM_IN: &str = "wasi:cli/terminal-input@0.2.0";
const CLI_TERM_OUT: &str = "wasi:cli/terminal-output@0.2.0";
const CLI_TERM_STDIN: &str = "wasi:cli/terminal-stdin@0.2.0";
const CLI_TERM_STDOUT: &str = "wasi:cli/terminal-stdout@0.2.0";
const CLI_TERM_STDERR: &str = "wasi:cli/terminal-stderr@0.2.0";
const MONOTONIC: &str = "wasi:clocks/monotonic-clock@0.2.0";
const WALL_CLOCK: &str = "wasi:clocks/wall-clock@0.2.0";
const IO_ERROR: &str = "wasi:io/error@0.2.0";
const IO_POLL: &str = "wasi:io/poll@0.2.0";
const IO_STREAMS: &str = "wasi:io/streams@0.2.0";
const FS_TYPES: &str = "wasi:filesystem/types@0.2.0";
const FS_PREOPENS: &str = "wasi:filesystem/preopens@0.2.0";
const RANDOM: &str = "wasi:random/random@0.2.0";

/// The three interfaces that carry a delegated entry, re-exported so a second
/// implementation of the adapter — the init sandbox runs one, against wizer's
/// wasmtime rather than the data plane's — matches on the same strings this
/// table is built from instead of retyping them.
pub const MONOTONIC_CLOCK: &str = MONOTONIC;
pub const CLI_ENVIRONMENT: &str = CLI_ENV;
pub const RANDOM_RANDOM: &str = RANDOM;

/// `wisp:platform` imports the adapter is allowed to draw on. Naming them as
/// constants keeps the delegation targets greppable and lets the tests assert
/// the set is exactly these two.
const CLOCK: Delegation = Delegation::Cap("clock_now_coarse");
const ENTROPY: Delegation = Delegation::Cap("entropy_fill");

/// A `TrapOnCall` entry, which is most of them.
const fn trap(module: &'static str, func: &'static str, why: &'static str) -> PalDef {
    PalDef { module, func, disposition: Disposition::TrapOnCall, sig: None, why }
}

/// A delegated entry. The signature is mandatory here, precisely because the
/// adapter implements a body against it.
const fn delegate(
    module: &'static str,
    func: &'static str,
    to: Delegation,
    sig: Sig,
    why: &'static str,
) -> PalDef {
    PalDef { module, func, disposition: Disposition::Delegate(to), sig: Some(sig), why }
}

/// **The complete WASI surface the platform will compose.** Anything outside
/// this table is a V1 rejection (§4.3).
pub const PAL_IMPORTS: &[PalDef] = &[
    // ---- delegated: the 2–3 imports F7 measured as actually reached ------
    // §2.5.4: "no ambient environment". There is no `wisp:platform`
    // capability to delegate to, and that IS the delegation: the adapter
    // answers with the empty list. Note what is deliberately NOT done here —
    // wiring this to `init_config_get` would hand deploy-time configuration
    // to the guest through a door the manifest does not describe.
    delegate(
        CLI_ENV,
        "get-environment",
        Delegation::Nothing,
        Sig { params: &[I32], results: &[] },
        "§2.5.4: there is no ambient environment, so the empty list is the \
         same-fidelity answer for a capability the platform does not have",
    ),
    // §8.4/§8.5. THE load-bearing delegation. `clock_now_coarse` is
    // millisecond-granular and frozen for the dispatch slice, and this import
    // inherits both properties: the value is the coarse clock multiplied into
    // nanoseconds, so the low 6 decimal digits are always zero and two calls
    // inside one slice return the same instant. §8.4 denies guests a fine
    // timer because it is the measurement primitive Spectre-class attacks
    // need; a delegation that leaked nanosecond resolution would silently
    // re-open the hole the whole design closes.
    delegate(
        MONOTONIC,
        "now",
        CLOCK,
        Sig { params: &[], results: &[I64] },
        "§8.4/§8.5: quantized to the coarse clock's 1 ms resolution and frozen \
         intra-slice — no finer timer than `clock_now_coarse` already is",
    ),
    // §4.3: "the resolution query is quantized to match". A guest that is told
    // the clock resolves to 1 ns and then observes 1 ms steps has learned
    // nothing extra, but a guest told 1 ns may build a spin-until-tick loop
    // that never terminates. Answering 1 ms is both truthful and kinder.
    delegate(
        MONOTONIC,
        "resolution",
        CLOCK,
        Sig { params: &[], results: &[I64] },
        "§4.3: the resolution query is quantized to match the delegated clock \
         (1 ms), so the reported fidelity equals the delivered fidelity",
    ),
    // Host generator state stays outside captured guest memory. Guest code can
    // retain returned bytes or a PRNG of its own in a template.
    // A failed draw reaches `_Exit` through `ObjectHeader.AssignHashCode`.
    delegate(
        RANDOM,
        "get-random-bytes",
        ENTROPY,
        Sig { params: &[I64, I32], results: &[] },
        "host CSPRNG drawn per call; guest code can retain returned bytes \
         or its own PRNG state in captured memory",
    ),
    // ---- trap-on-call: cli ------------------------------------------------
    // §13: a trap fails the dispatch and discards the instance. That is the
    // right answer for every one of these — the declaration is toolchain
    // residue, the capability is denied, and if a toolchain bump ever reaches
    // one the failure is loud instead of silent.
    trap(CLI_EXIT, "exit", "§6.4/§13: process exit is the host's decision, not the guest's"),
    trap(CLI_STDIN, "get-stdin", "§2.5.4: no ambient I/O; observability is `log_emit`"),
    trap(CLI_STDOUT, "get-stdout", "§2.5.4/§10: stdout is not a log sink; `log_emit` is"),
    trap(CLI_STDERR, "get-stderr", "§2.5.4/§10: stderr is not a log sink; `log_emit` is"),
    trap(CLI_TERM_IN, "[resource-drop]terminal-input", "no terminal exists to drop a handle to"),
    trap(CLI_TERM_OUT, "[resource-drop]terminal-output", "no terminal exists to drop a handle to"),
    trap(CLI_TERM_STDIN, "get-terminal-stdin", "§2.5.4: there is no terminal"),
    trap(CLI_TERM_STDOUT, "get-terminal-stdout", "§2.5.4: there is no terminal"),
    trap(CLI_TERM_STDERR, "get-terminal-stderr", "§2.5.4: there is no terminal"),
    // ---- trap-on-call: clocks --------------------------------------------
    // §4.3 suggests `subscribe-duration` "maps to host deadline futures, not
    // pollables", and in a component-model world that is exactly right. In
    // this PoC's flat core-wasm lowering it is NOT reachable honestly: the
    // only thing a guest can do with the returned `pollable` is hand it to
    // `wasi:io/poll::poll` or `[method]pollable.block`, both of which BLOCK —
    // and "there is no import that waits" (§5.1) is what makes invariant 3
    // hold by construction. Delegating the subscribe half while trapping the
    // block half would hand the guest a handle whose only use is a trap, so
    // both halves trap and the diagnostic names the real reason.
    trap(
        MONOTONIC,
        "subscribe-instant",
        "§5.1/§8.4: returns a pollable whose only consumer is a blocking wait; \
         the WISP equivalent is the `clock_deadline_in` FUTURE, which cannot be \
         expressed as a pollable without reintroducing a wait",
    ),
    trap(
        MONOTONIC,
        "subscribe-duration",
        "§5.1/§8.4: returns a pollable whose only consumer is a blocking wait; \
         the WISP equivalent is the `clock_deadline_in` FUTURE, which cannot be \
         expressed as a pollable without reintroducing a wait",
    ),
    // A wall clock is a second time source with its own drift, and §8.5
    // specifies skew tolerance for exactly one clock. Two clocks whose
    // difference is observable is a channel `wisp:platform` does not have.
    trap(
        WALL_CLOCK,
        "now",
        "§8.5 specifies ONE clock; a second time source whose difference from \
         the first is observable is a new channel",
    ),
    // ---- trap-on-call: io -------------------------------------------------
    trap(IO_ERROR, "[resource-drop]error", "no stream exists to have produced an error"),
    trap(IO_POLL, "[resource-drop]pollable", "no pollable is ever handed out"),
    trap(
        IO_POLL,
        "[method]pollable.block",
        "§5.1: there is no import that waits — this one IS the wait, and \
         admitting it would break invariant 3 by construction",
    ),
    trap(IO_STREAMS, "[resource-drop]input-stream", "no stream is ever handed out"),
    trap(IO_STREAMS, "[resource-drop]output-stream", "no stream is ever handed out"),
    trap(IO_STREAMS, "[method]input-stream.blocking-read", "§5.1: no import waits"),
    trap(IO_STREAMS, "[method]input-stream.subscribe", "no stream is ever handed out"),
    trap(IO_STREAMS, "[method]output-stream.check-write", "no stream is ever handed out"),
    trap(IO_STREAMS, "[method]output-stream.write", "§2.5.4: no ambient I/O"),
    trap(IO_STREAMS, "[method]output-stream.blocking-flush", "§5.1: no import waits"),
    trap(IO_STREAMS, "[method]output-stream.subscribe", "no stream is ever handed out"),
    // ---- trap-on-call: filesystem ----------------------------------------
    // The substantively worst group, and the one F7 confirmed is entirely
    // dead. `wasi:filesystem` is ambient filesystem authority, which §2.5.4
    // denies outright; there is no `wisp:platform` capability it could
    // delegate to at any fidelity. `get-directories` traps rather than
    // returning an empty preopen list, because an empty list is a valid
    // answer that lets the runtime proceed as if a filesystem existed — a
    // trap says what is true.
    trap(FS_PREOPENS, "get-directories", "§2.5.4: no filesystem, ambient or otherwise"),
    trap(FS_TYPES, "[resource-drop]descriptor", "no descriptor is ever handed out"),
    trap(FS_TYPES, "[resource-drop]directory-entry-stream", "no directory is ever opened"),
    trap(FS_TYPES, "[method]descriptor.read-via-stream", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.write-via-stream", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.append-via-stream", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.advise", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.get-flags", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.read", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.read-directory", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.stat", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.stat-at", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.open-at", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.readlink-at", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.unlink-file-at", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.metadata-hash", "§2.5.4: no filesystem"),
    trap(FS_TYPES, "[method]descriptor.metadata-hash-at", "§2.5.4: no filesystem"),
    trap(
        FS_TYPES,
        "[method]directory-entry-stream.read-directory-entry",
        "§2.5.4: no filesystem",
    ),
];

/// Is `module` a WASI core-module name at all? Used to phrase the V1
/// rejection: "not in the PAL whitelist" is a different sentence from "not in
/// the capability table", and a tenant deserves the right one.
pub fn is_wasi(module: &str) -> bool {
    module.starts_with(WASI_PREFIX)
}

/// The whitelist lookup. `None` means V1 rejection — there is no wildcard.
pub fn lookup(module: &str, func: &str) -> Option<&'static PalDef> {
    PAL_IMPORTS.iter().find(|p| p.module == module && p.func == func)
}

pub fn delegated() -> impl Iterator<Item = &'static PalDef> {
    PAL_IMPORTS.iter().filter(|p| p.disposition.is_delegated())
}

/// The distinct `wisp:platform` capabilities reachable through the adapter.
/// This is the set the no-new-channels rule is *about*.
pub fn delegation_targets() -> Vec<&'static str> {
    let mut v: Vec<&'static str> =
        PAL_IMPORTS.iter().filter_map(|p| p.disposition.capability()).collect();
    v.sort_unstable();
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// the adapter itself
// ---------------------------------------------------------------------------

/// One millisecond in nanoseconds — the quantum of the delegated monotonic
/// clock, and the value `resolution()` reports.
pub const CLOCK_QUANTUM_NS: i64 = 1_000_000;

/// Ceiling on one `get-random-bytes` draw. §11.1 caps the *guest's* memory;
/// this caps what the guest can make the host allocate on its behalf, and
/// mirrors the engine's `MAX_OUT_BUFFER`.
pub const MAX_ENTROPY_BYTES: u64 = 1 << 20;

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

    let mut composed = Vec::new();
    for import in module.imports() {
        if import.module() == imports::HOST_MODULE {
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
                (ms as i64).saturating_mul(CLOCK_QUANTUM_NS)
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
