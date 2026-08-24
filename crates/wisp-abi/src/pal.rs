//! The canonical table of admitted WASI imports and their dispositions.
//! Validation and linking read this table; delegated entries name their host capability.

use crate::imports::{self, Sig, Ty};

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
    /// `docs/dotnet-findings.md`, in violation messages, and in the manifest.
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
