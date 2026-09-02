//! The WASI adapter table. NativeAOT-LLVM output declares `wasi:*` imports
//! the capability table would reject, and reaches only a handful of them. At
//! deploy the platform composes an adapter: each whitelisted import is either
//! delegated to a `wisp:platform` import at the same fidelity, or satisfied
//! by a stub that traps when called, so the declaration validates while the
//! capability stays absent. No WASI import may add a channel, time, entropy
//! or otherwise, that `wisp:platform` does not already offer.
//!
//! The table is sized to the closure of imports the toolchain emits for the
//! guests in this tree, read out of the built modules with the guest-side PAL
//! object disabled (`guests/dotnet/build.sh -p:WispPalObject=`), not to a
//! hello-world module: pulling in LINQ, source-generated `Regex` or
//! `System.Text.Json` drags further filesystem and stream imports out of the
//! BCL. It is still enumerated; the next unlisted import is a rejection.
//! `monotonic-clock::resolution` is the one entry with no observed provenance,
//! listed so a toolchain that starts asking gets the quantized answer.

use crate::imports::{Sig, Ty};

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

pub const WASI_PREFIX: &str = "wasi:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegation {
    /// The `wisp:platform` import that supplies the value.
    Cap(&'static str),
    /// The adapter answers with the empty value, because the platform has no
    /// such capability and an empty answer is the same fidelity as absent.
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Delegate(Delegation),
    /// A stub that traps when called: the declaration validates, the
    /// capability is absent.
    TrapOnCall,
}

impl Disposition {
    pub fn is_delegated(&self) -> bool {
        matches!(self, Disposition::Delegate(_))
    }
    /// `None` for both `TrapOnCall` and [`Delegation::Nothing`].
    pub fn capability(&self) -> Option<&'static str> {
        match self {
            Disposition::Delegate(Delegation::Cap(c)) => Some(c),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PalDef {
    /// Core-wasm module name, `wasi:random/random@0.2.0`.
    pub module: &'static str,
    /// Core-wasm field name in canonical-ABI spelling, `get-random-bytes` or
    /// `[method]descriptor.stat`.
    pub func: &'static str,
    pub disposition: Disposition,
    /// Present only for a delegated entry, whose body is typed against it. A
    /// trap stub is generated from whatever type the module declares, so a
    /// pinned shape would break the first time a toolchain lowered the
    /// function differently while guarding nothing.
    pub sig: Option<Sig>,
    /// Why this disposition, for the manifest a tenant reads.
    pub why: &'static str,
}

impl PalDef {
    /// `module::func`, the spelling used in violation messages and manifests.
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

/// The interfaces that carry a delegated entry, named for the init sandbox's
/// adapter so it matches on the strings this table is built from.
pub const MONOTONIC_CLOCK: &str = MONOTONIC;
pub const CLI_ENVIRONMENT: &str = CLI_ENV;
pub const RANDOM_RANDOM: &str = RANDOM;

const CLOCK: Delegation = Delegation::Cap("clock_now_coarse");
const ENTROPY: Delegation = Delegation::Cap("entropy_fill");

const fn trap(module: &'static str, func: &'static str, why: &'static str) -> PalDef {
    PalDef {
        module,
        func,
        disposition: Disposition::TrapOnCall,
        sig: None,
        why,
    }
}

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

pub const PAL_IMPORTS: &[PalDef] = &[
    // ---- delegated ----------------------------------------------------------
    // Not wired to `init_config_get`: that would hand deploy-time
    // configuration to the guest through a door the manifest does not name.
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
    // Inherits the coarse clock's millisecond quantum and intra-slice freeze:
    // the value is the coarse clock multiplied into nanoseconds. A finer
    // timer is the measurement primitive Spectre-class attacks need.
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
    // A guest told 1 ns and then shown 1 ms steps may spin until a tick that
    // never comes inside its slice.
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
    // Not optional: a failed draw reaches `_Exit` through
    // `ObjectHeader.AssignHashCode`, so a guest with no entropy does not run.
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
    // The subscribe half could map onto a deadline future, but the only use
    // of the returned pollable is a blocking wait, so both halves trap rather
    // than hand out a handle whose only use is a trap.
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
    // Emitted by managed code through `WasmImportLinkage` when enough of
    // `System.Threading` is rooted, so unlike the wasi-libc imports there is
    // no C symbol the guest-side shim could define to make it go away.
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
    // `get-directories` traps rather than returning an empty preopen list,
    // because an empty list lets the runtime proceed as if a filesystem
    // existed.
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

/// Whether `module` is a WASI interface at all, so a rejection can say "not in
/// the PAL whitelist" rather than "not in the capability table".
pub fn is_wasi(module: &str) -> bool {
    module.starts_with(WASI_PREFIX)
}

pub fn lookup(module: &str, func: &str) -> Option<&'static PalDef> {
    PAL_IMPORTS
        .iter()
        .find(|p| p.module == module && p.func == func)
}

pub fn delegated() -> impl Iterator<Item = &'static PalDef> {
    PAL_IMPORTS.iter().filter(|p| p.disposition.is_delegated())
}

/// The distinct `wisp:platform` capabilities reachable through the adapter.
pub fn delegation_targets() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = PAL_IMPORTS
        .iter()
        .filter_map(|p| p.disposition.capability())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The quantum of the delegated monotonic clock, and what `resolution` reports.
pub const CLOCK_QUANTUM_NS: i64 = 1_000_000;

/// Ceiling on one `get-random-bytes` draw.
pub const MAX_ENTROPY_BYTES: u64 = crate::imports::MAX_OUT_BUFFER_BYTES as u64;
