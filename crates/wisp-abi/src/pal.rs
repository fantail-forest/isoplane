//! The **WASI PAL adapter** — deploy-time composition.
//!
//! NativeAOT-LLVM/componentize artifacts declare `wasi:*` imports that raw V1
//! would reject, and only a handful of them are ever reached. Rather than
//! fight the toolchain, the platform composes a **WASI compatibility adapter**
//! at deploy: an enumerated whitelist of `wasi:*` interfaces satisfied by
//! delegation to `wisp:platform` imports, and trap-on-call stubs for the
//! unreached remainder, so validation passes while the capability stays
//! absent.
//!
//! **No-new-channels rule:** no WASI import may introduce a capability or
//! information channel — time, entropy, or otherwise — that is not already
//! present, **at the same fidelity**, in `wisp:platform`.
//!
//! ## Why this is one table
//!
//! [`PAL_IMPORTS`] is to the WASI surface what [`crate::imports::IMPORTS`]
//! is to the WISP surface: the single machine-readable statement of what may
//! cross the boundary. Three consumers read it and must not disagree —
//!
//! * the **validator** (`wisp_control::validator`, rule V1): a `wasi:*` import in
//!   this table is admissible; anything else is still an attempted capability
//!   and still a rejection. There is no wildcard and no "the toolchain
//!   probably needs it" clause.
//! * the **linker** (`wisp_control::pal::link`): composes exactly these and nothing else. A
//!   delegated entry gets the delegating body; a `TrapOnCall` entry gets a
//!   stub that traps, so the declaration exists and the capability does not.
//! * the **auditor**: `report.manifest()` prints every delegated import
//!   *as the `wisp:platform` capability it resolves to*. A capability that
//!   entered through a WASI door must show up in the tenant-facing manifest;
//!   silently vanishing is the failure mode this table exists to prevent.
//!
//! ## Provenance of the entries
//!
//! Not guessed. Every entry below was read out of a module the real toolchain
//! produced, built with the guest-side C PAL disabled:
//!
//! ```text
//! WISP_OUT=wisp_guest_dotnet_stock guests/dotnet/build.sh -p:WispPalObject=
//! WISP_OUT=wisp_guest_dotnet_compat_stock guests/dotnet/build.sh \
//!    -p:WispPalObject= -p:WispCompatProbe=true
//! ```
//!
//! The second build is the reason the table is not sized to the first: **the
//! WASI surface is a closure over what tenant code drags out of the BCL, not a
//! fixed list** — adding LINQ, source-generated `Regex` and `System.Text.Json`
//! pulls in further filesystem and stream imports. A whitelist sized to the
//! hello-world module would reject the second app, so the table is sized to
//! the observed closure and *still* enumerated: the next unlisted import is a
//! V1 rejection and a design conversation, exactly like an unlisted entry in
//! `IMPORTS` would be.
//!
//! The one entry with no observed provenance is
//! `monotonic-clock::resolution`. It is here so that a toolchain that starts
//! asking gets the quantized answer rather than a deploy rejection.

use crate::imports::{Sig, Ty};

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// Every WISP import lives under core-wasm module `wisp`; every WASI import
/// under a module name beginning with this.
pub const WASI_PREFIX: &str = "wasi:";

/// What a delegated WASI import actually draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegation {
    /// The `wisp:platform` import that supplies the value. MUST name an entry
    /// in [`crate::imports::IMPORTS`] — you cannot delegate to a capability
    /// the platform does not have.
    Cap(&'static str),
    /// There is **no** `wisp:platform` capability to delegate to, and that is
    /// the answer: the adapter returns the empty value. Same fidelity as
    /// absent, which is what the no-new-channels rule asks for. Used only
    /// where the platform denies the capability outright — there is no
    /// ambient environment to report.
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
    /// For a delegated import the shape matters: the adapter runs a
    /// body against it, and a mismatch would be a type confusion reaching
    /// host code — the same argument `wisp_control::validator` makes for the
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
    /// violation messages and in the manifest.
    pub fn full_name(&self) -> String {
        format!("{}::{}", self.module, self.func)
    }
}

pub const CLI_ENV: &str = "wasi:cli/environment@0.2.0";
pub const CLI_EXIT: &str = "wasi:cli/exit@0.2.0";
pub const CLI_STDIN: &str = "wasi:cli/stdin@0.2.0";
pub const CLI_STDOUT: &str = "wasi:cli/stdout@0.2.0";
pub const CLI_STDERR: &str = "wasi:cli/stderr@0.2.0";
pub const CLI_TERM_IN: &str = "wasi:cli/terminal-input@0.2.0";
pub const CLI_TERM_OUT: &str = "wasi:cli/terminal-output@0.2.0";
pub const CLI_TERM_STDIN: &str = "wasi:cli/terminal-stdin@0.2.0";
pub const CLI_TERM_STDOUT: &str = "wasi:cli/terminal-stdout@0.2.0";
pub const CLI_TERM_STDERR: &str = "wasi:cli/terminal-stderr@0.2.0";
pub const MONOTONIC: &str = "wasi:clocks/monotonic-clock@0.2.0";
pub const WALL_CLOCK: &str = "wasi:clocks/wall-clock@0.2.0";
pub const IO_ERROR: &str = "wasi:io/error@0.2.0";
pub const IO_POLL: &str = "wasi:io/poll@0.2.0";
pub const IO_STREAMS: &str = "wasi:io/streams@0.2.0";
pub const FS_TYPES: &str = "wasi:filesystem/types@0.2.0";
pub const FS_PREOPENS: &str = "wasi:filesystem/preopens@0.2.0";
pub const RANDOM: &str = "wasi:random/random@0.2.0";

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
    PalDef {
        module,
        func,
        disposition: Disposition::TrapOnCall,
        sig: None,
        why,
    }
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
    PalDef {
        module,
        func,
        disposition: Disposition::Delegate(to),
        sig: Some(sig),
        why,
    }
}

/// **The complete WASI surface the platform will compose.** Anything outside
/// this table is a V1 rejection.
pub const PAL_IMPORTS: &[PalDef] = &[
    // ---- delegated: the handful of imports a guest actually reaches ------
    // There is no ambient environment, so there is no `wisp:platform`
    // capability to delegate to, and that IS the delegation: the adapter
    // answers with the empty list. Note what is deliberately NOT done here —
    // wiring this to `init_config_get` would hand deploy-time configuration
    // to the guest through a door the manifest does not describe.
    delegate(
        CLI_ENV,
        "get-environment",
        Delegation::Nothing,
        Sig {
            params: &[I32],
            results: &[],
        },
        "there is no ambient environment, so the empty list is the \
         same-fidelity answer for a capability the platform does not have",
    ),
    // THE critical delegation. `clock_now_coarse` is millisecond-granular and
    // frozen for the dispatch slice, and this import inherits both properties:
    // the value is the coarse clock multiplied into nanoseconds, so the low 6
    // decimal digits are always zero and two calls inside one slice return the
    // same instant. Guests get no fine timer because it is the measurement
    // primitive Spectre-class attacks need; a delegation that leaked
    // nanosecond resolution would silently re-open the hole the whole design
    // closes.
    delegate(
        MONOTONIC,
        "now",
        CLOCK,
        Sig {
            params: &[],
            results: &[I64],
        },
        "quantized to the coarse clock's 1 ms resolution and frozen \
         intra-slice — no finer timer than `clock_now_coarse` already is",
    ),
    // The resolution query is quantized to match. A guest that is told the
    // clock resolves to 1 ns and then observes 1 ms steps has learned
    // nothing extra, but a guest told 1 ns may build a spin-until-tick loop
    // that never terminates. Answering 1 ms is both truthful and kinder.
    delegate(
        MONOTONIC,
        "resolution",
        CLOCK,
        Sig {
            params: &[],
            results: &[I64],
        },
        "the resolution query is quantized to match the delegated clock \
         (1 ms), so the reported fidelity equals the delivered fidelity",
    ),
    // Host generator state stays outside captured guest memory. Guest code can
    // retain returned bytes or a PRNG of its own in a template.
    // A failed draw reaches `_Exit` through `ObjectHeader.AssignHashCode`.
    delegate(
        RANDOM,
        "get-random-bytes",
        ENTROPY,
        Sig {
            params: &[I64, I32],
            results: &[],
        },
        "host CSPRNG drawn per call; guest code can retain returned bytes \
         or its own PRNG state in captured memory",
    ),
    // ---- trap-on-call: cli ------------------------------------------------
    // A trap fails the dispatch and discards the instance. That is the
    // right answer for every one of these — the declaration is toolchain
    // residue, the capability is denied, and if a toolchain bump ever reaches
    // one the failure is loud instead of silent.
    trap(
        CLI_EXIT,
        "exit",
        "process exit is the host's decision, not the guest's",
    ),
    trap(
        CLI_STDIN,
        "get-stdin",
        "no ambient I/O; observability is `log_emit`",
    ),
    trap(
        CLI_STDOUT,
        "get-stdout",
        "stdout is not a log sink; `log_emit` is",
    ),
    trap(
        CLI_STDERR,
        "get-stderr",
        "stderr is not a log sink; `log_emit` is",
    ),
    trap(
        CLI_TERM_IN,
        "[resource-drop]terminal-input",
        "no terminal exists to drop a handle to",
    ),
    trap(
        CLI_TERM_OUT,
        "[resource-drop]terminal-output",
        "no terminal exists to drop a handle to",
    ),
    trap(
        CLI_TERM_STDIN,
        "get-terminal-stdin",
        "there is no terminal to hand out",
    ),
    trap(
        CLI_TERM_STDOUT,
        "get-terminal-stdout",
        "there is no terminal to hand out",
    ),
    trap(
        CLI_TERM_STDERR,
        "get-terminal-stderr",
        "there is no terminal to hand out",
    ),
    // ---- trap-on-call: clocks --------------------------------------------
    // `subscribe-duration` maps naturally onto host deadline futures rather
    // than pollables, and in a component-model world that would be exactly
    // right. In this PoC's flat core-wasm lowering it is NOT reachable
    // honestly: the only thing a guest can do with the returned `pollable` is
    // hand it to `wasi:io/poll::poll` or `[method]pollable.block`, both of
    // which BLOCK — and having no import that waits is what makes invariant 3
    // hold by construction. Delegating the subscribe half while trapping the
    // block half would hand the guest a handle whose only use is a trap, so
    // both halves trap and the diagnostic names the real reason.
    trap(
        MONOTONIC,
        "subscribe-instant",
        "returns a pollable whose only consumer is a blocking wait; \
         the WISP equivalent is the `clock_deadline_in` FUTURE, which cannot be \
         expressed as a pollable without reintroducing a wait",
    ),
    trap(
        MONOTONIC,
        "subscribe-duration",
        "returns a pollable whose only consumer is a blocking wait; \
         the WISP equivalent is the `clock_deadline_in` FUTURE, which cannot be \
         expressed as a pollable without reintroducing a wait",
    ),
    // A wall clock is a second time source with its own drift, and the
    // platform bounds skew for exactly one clock. Two clocks whose
    // difference is observable is a channel `wisp:platform` does not have.
    trap(
        WALL_CLOCK,
        "now",
        "the platform has ONE clock; a second time source whose difference from \
         the first is observable is a new channel",
    ),
    // ---- trap-on-call: io -------------------------------------------------
    trap(
        IO_ERROR,
        "[resource-drop]error",
        "no stream exists to have produced an error",
    ),
    trap(
        IO_POLL,
        "[resource-drop]pollable",
        "no pollable is ever handed out",
    ),
    trap(
        IO_POLL,
        "[method]pollable.block",
        "there is no import that waits — this one IS the wait, and \
         admitting it would break invariant 3 by construction",
    ),
    // Refuse this potentially blocking wait during a guest invocation.
    trap(
        IO_POLL,
        "poll",
        "there is no import that waits — this is `pollable.block` over a \
         list, and admitting it would break invariant 3 by construction",
    ),
    trap(
        IO_STREAMS,
        "[resource-drop]input-stream",
        "no stream is ever handed out",
    ),
    trap(
        IO_STREAMS,
        "[resource-drop]output-stream",
        "no stream is ever handed out",
    ),
    trap(
        IO_STREAMS,
        "[method]input-stream.blocking-read",
        "no import waits, no stream exists",
    ),
    trap(
        IO_STREAMS,
        "[method]input-stream.subscribe",
        "no stream is ever handed out",
    ),
    trap(
        IO_STREAMS,
        "[method]output-stream.check-write",
        "no stream is ever handed out",
    ),
    trap(
        IO_STREAMS,
        "[method]output-stream.write",
        "no ambient I/O; observability is `log_emit`",
    ),
    trap(
        IO_STREAMS,
        "[method]output-stream.blocking-flush",
        "no import waits, no stream exists",
    ),
    trap(
        IO_STREAMS,
        "[method]output-stream.subscribe",
        "no stream is ever handed out",
    ),
    // ---- trap-on-call: filesystem ----------------------------------------
    // The substantively worst group, and the one the probe builds confirmed
    // is entirely dead. `wasi:filesystem` is ambient filesystem authority,
    // which the platform denies outright; there is no capability it could
    // delegate to at any fidelity. `get-directories` traps rather than
    // returning an empty preopen list, because an empty list is a valid
    // answer that lets the runtime proceed as if a filesystem existed — a
    // trap says what is true.
    trap(
        FS_PREOPENS,
        "get-directories",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[resource-drop]descriptor",
        "no descriptor is ever handed out",
    ),
    trap(
        FS_TYPES,
        "[resource-drop]directory-entry-stream",
        "no directory is ever opened",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.read-via-stream",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.write-via-stream",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.append-via-stream",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.advise",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.get-flags",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.read",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.read-directory",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.stat",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.stat-at",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.open-at",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.readlink-at",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.unlink-file-at",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.metadata-hash",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]descriptor.metadata-hash-at",
        "no filesystem, ambient or otherwise",
    ),
    trap(
        FS_TYPES,
        "[method]directory-entry-stream.read-directory-entry",
        "no filesystem, ambient or otherwise",
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
    PAL_IMPORTS
        .iter()
        .find(|p| p.module == module && p.func == func)
}

pub fn delegated() -> impl Iterator<Item = &'static PalDef> {
    PAL_IMPORTS.iter().filter(|p| p.disposition.is_delegated())
}

/// The distinct `wisp:platform` capabilities reachable through the adapter.
/// This is the set the no-new-channels rule is *about*.
pub fn delegation_targets() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = PAL_IMPORTS
        .iter()
        .filter_map(|p| p.disposition.capability())
        .collect();
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

/// Ceiling on one `get-random-bytes` draw. The plan limits cap the *guest's*
/// memory; this caps what the guest can make the host allocate for it, and
/// mirrors the engine's `MAX_OUT_BUFFER`.
pub const MAX_ENTROPY_BYTES: u64 = 1 << 20;
