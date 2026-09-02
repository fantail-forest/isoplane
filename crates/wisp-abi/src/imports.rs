//! Canonical guest capabilities consumed by the validator, linker and SDK.
//! This is the shared definition used to check declared imports and signatures.

/// Core-wasm module name that every WISP import lives under.
pub const HOST_MODULE: &str = "wisp";

/// The guest exports the host requires.
pub mod exports {
    pub const ALLOC: &str = "wisp_alloc";
    pub const HANDLE: &str = "wisp_handle";
    pub const WAKE: &str = "wisp_wake";
    pub const GC_HINT: &str = "wisp_gc_hint";
    pub const INITIALIZE: &str = "wisp_initialize";
    pub const MEMORY: &str = "memory";

    /// Required for a module to be deployable at all.
    pub const REQUIRED: &[&str] = &[ALLOC, HANDLE, WAKE, MEMORY];
    /// Optional but strongly recommended.
    pub const OPTIONAL: &[&str] = &[GC_HINT, INITIALIZE];

    use super::{Sig, Ty::I32, Ty::I64};

    /// Core-wasm signatures, for the same reason [`super::IMPORTS`] exists:
    /// the validator, the linker and the guest SDK must check ONE definition
    /// rather than three copies of a paragraph.
    ///
    /// `(ptr, len)` pairs always address a postcard-encoded `DispatchRecord`
    /// or `ResultRecord` in guest memory. `wisp_handle` and `wisp_wake`
    /// return a packed `PollStatus` (see `crate::status`).
    pub const SIG_ALLOC: Sig = Sig { params: &[I32, I32], results: &[I32] };
    pub const SIG_HANDLE: Sig = Sig { params: &[I32, I32], results: &[I64] };
    pub const SIG_WAKE: Sig = Sig { params: &[I64, I32, I32], results: &[I64] };
    pub const SIG_GC_HINT: Sig = Sig { params: &[I32], results: &[] };
    pub const SIG_INITIALIZE: Sig = Sig { params: &[], results: &[] };

    /// `(name, signature)` for every export the host may call.
    pub const SIGNATURES: &[(&str, Sig)] = &[
        (ALLOC, SIG_ALLOC),
        (HANDLE, SIG_HANDLE),
        (WAKE, SIG_WAKE),
        (GC_HINT, SIG_GC_HINT),
        (INITIALIZE, SIG_INITIALIZE),
    ];

    pub fn signature(name: &str) -> Option<Sig> {
        SIGNATURES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    I32,
    I64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sig {
    pub params: &'static [Ty],
    pub results: &'static [Ty],
}


/// Position and direction of an import buffer; scalar parameters do not describe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufSpec {
    /// No buffer; every parameter is a scalar.
    None,
    /// Guest -> host. `(params[i], params[i+1])` is `(ptr, len)`, read out of
    /// guest memory and valid only for the duration of the call.
    In(u8),
    /// Host -> guest. `(params[i], params[i+1])` is `(ptr, cap)`; the host
    /// fills a scratch buffer that the engine then copies in. Still no host
    /// pointer ever enters guest memory.
    Out(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// Returns a value immediately and cannot block. Safe intra-slice.
    Sync,
    /// Returns a `future-id: u64` immediately. There is no import that waits,
    /// which is what makes invariant 3 hold by construction.
    Future,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Runtime plumbing: allocation handshake, panics, pre-init interposer.
    Core,
    Observe,
    Clock,
    Entropy,
    Http,
    /// Building the inbound reply.
    Response,
    Ctx,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportDef {
    pub name: &'static str,
    pub group: Group,
    pub sig: Sig,
    pub kind: ImportKind,
    /// Where the `(ptr, len)` pair is, and which way it flows.
    pub buf: BufSpec,
    /// Marks values that require sensitive-data handling.
    pub sensitive: bool,
    /// Included in the baseline initialization import set.
    pub init_visible: bool,
    /// What this capability is *for*, in one sentence, for a tenant reading
    /// the manifest rather than the table.
    ///
    /// Here rather than in the documentation generator for the same reason
    /// [`PalDef::why`](crate::pal::PalDef::why) is here: the capability
    /// surface has one definition, and a reference page that re-describes it
    /// somewhere else is a second one that goes stale silently. The `imp!`
    /// macro takes it as a required parameter, so a new capability cannot be
    /// added without saying what it is for.
    ///
    /// Read by `wisp doctor` when it explains a manifest.
    pub why: &'static str,
}

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// `(ptr: i32, len: i32) -> i64` — the shape of every postcard-encoded,
/// future-returning import. Guest memory is read at (ptr, len), which is
/// valid only for the duration of the call.
const BUF_TO_FUTURE: Sig = Sig { params: &[I32, I32], results: &[I64] };
const BUF_TO_VOID: Sig = Sig { params: &[I32, I32], results: &[] };
const BUF_TO_I32: Sig = Sig { params: &[I32, I32], results: &[I32] };

macro_rules! imp {
    ($name:literal, $group:ident, $sig:expr, $kind:expr, $why:literal) => {
        ImportDef {
            name: $name,
            group: Group::$group,
            sig: $sig,
            kind: $kind,
            buf: BufSpec::In(0),
            sensitive: false,
            init_visible: false,
            why: $why,
        }
    };
}

const SYNC: ImportKind = ImportKind::Sync;
const FUT: ImportKind = ImportKind::Future;

/// **The complete capability surface.** Nothing else may be imported.
pub const IMPORTS: &[ImportDef] = &[
    // ---- core -------------------------------------------------------------
    // Guest panic. Available at init so a failed pre-init reports usefully.
    ImportDef { init_visible: true, ..imp!("abort", Core, BUF_TO_VOID, SYNC,
        "guest panic; available at init so a failed pre-initialization reports usefully rather than \
         trapping anonymously") },
    // A forbidden unit causes a bounded retry that defers that unit.
    // Earlier allowed external effects are not rolled back.
    ImportDef {
        init_visible: true,
        ..ImportDef { buf: BufSpec::None, ..imp!("init_unit_begin", Core, Sig { params: &[I32], results: &[] }, SYNC,
            "names the init unit about to run, so a trap inside it can be attributed to that unit \
             and deferred") }
    },
    ImportDef {
        init_visible: true,
        ..ImportDef { buf: BufSpec::None, ..imp!("init_unit_deferred", Core, Sig { params: &[I32], results: &[I32] }, SYNC,
            "asks whether this unit was deferred on a previous attempt; non-zero means skip it on \
             the replay") }
    },
    // The preparation string-hash seed is separate from entropy requests and can be captured in guest state.
    ImportDef {
        init_visible: true,
        sensitive: true,
        ..ImportDef { buf: BufSpec::Out(0), ..imp!("init_hash_seed", Core, BUF_TO_VOID, SYNC,
            "A per-attempt string-hash seed, separate from host entropy requests.") }
    },

    // A template deletes the config parse from cold activation, and the
    // tenant declares `init_visible_vars` — but without this import the blob
    // has no way to reach the guest. Unlike the hash seed this is NOT
    // externally sourced: it is tenant-authored deploy input, fixed for the
    // version and part of the reproducibility preimage, so pre-running a
    // config parse bakes no shared secret and no shared entropy.
    // (ptr, cap) -> i32 returns the blob's FULL length, so a guest can size
    // its buffer with a zero-length probe.
    ImportDef {
        init_visible: true,
        ..ImportDef { buf: BufSpec::Out(0), ..imp!("init_config_get", Core, BUF_TO_I32, SYNC,
            "the init-visible config blob, so a config parse happens once at deploy time instead of \
             once per activation; returns the blob's FULL length, so a guest can size its buffer \
             with a zero-length probe") }
    },

    // ---- observability ----------------------------------------------------
    // Log CPU counts against the dispatch budget, which is what stops
    // log-as-compute abuse.
    ImportDef { buf: BufSpec::In(1), init_visible: true, ..imp!("log_emit", Observe, Sig { params: &[I32, I32, I32], results: &[] }, SYNC,
        "structured log at `trace=0` .. `error=4`; the CPU counts against the dispatch budget, \
         which is what stops log-as-compute abuse") },
    imp!("trace_span_begin", Observe, BUF_TO_I32, SYNC,
        "opens a trace span and returns its id, for correlating work inside one dispatch"),
    ImportDef { buf: BufSpec::None, ..imp!("trace_span_end", Observe, Sig { params: &[I32], results: &[] }, SYNC,
        "closes the span with that id; unbalanced spans are the guest's problem, not the host's") },

    // ---- clock & entropy --------------------------------------------------
    // Coarse (1 ms) and FROZEN intra-slice: removes the
    // measurement primitive most Spectre-class attacks need. NOT init-visible
    // — a cctor that reads the clock is impure and gets deferred.
    ImportDef { buf: BufSpec::None, ..imp!("clock_now_coarse", Clock, Sig { params: &[], results: &[I64] }, SYNC,
        "Reads the host coarse clock; use host deadline futures to wait.") },
    ImportDef { buf: BufSpec::None, ..imp!("clock_deadline_in", Clock, Sig { params: &[I64], results: &[I64] }, FUT,
        "a timer as a host future, so waiting is never a guest loop burning budget") },
    // Requests bytes through the host entropy interface; captured guest values remain captured.
    ImportDef { buf: BufSpec::Out(0), ..imp!("entropy_fill", Entropy, BUF_TO_VOID, SYNC,
        "Host CSPRNG bytes for this request; values already stored in guest memory remain part of captured state.") },

    // ---- outbound http ----------------------------------
    imp!("http_fetch", Http, BUF_TO_FUTURE, FUT,
        "Issues an outbound request through the host egress policy and completes with its response head."),
    // A response-body read completes through a host-owned future.
    ImportDef { buf: BufSpec::None, ..imp!("http_body_read", Http, Sig { params: &[I32, I32], results: &[I64] }, FUT,
        "Reads the next chunk of a response body through a host-owned future.") },

    // ---- inbound response -------------------------------------------------
    imp!("resp_new", Response, BUF_TO_I32, SYNC,
        "opens a response from a `ResponseHead` and returns its handle"),
    ImportDef { buf: BufSpec::In(1), ..imp!("resp_write", Response, Sig { params: &[I32, I32, I32], results: &[I64] }, FUT,
        "appends a streamed chunk to an open response") },
    ImportDef { buf: BufSpec::None, ..imp!("resp_finish", Response, Sig { params: &[I32], results: &[] }, SYNC,
        "ends the response, after which the handle is spent") },




    // Imports marked as carrying sensitive data.


    // ---- the kill-cooperation ABI ------------------------------------------
    // Two halves of one mechanism, and both are engine-owned: neither ever
    // reaches a `HostBindings` vtable, because the budget state they read and
    // write lives in the engine's store, next to the meter the epoch callback
    // compares against.
    //
    // `budget_remaining_ns` is the host-published remaining-budget value the
    // runtime layer reads so it can raise `BudgetExceededException` at its
    // natural boundaries — the allocation slow path, GC entry, and task
    // switch. It returns the signed nanoseconds left against the budget; once
    // grace has begun it is <= 0 and keeps falling, so a guest can tell "at
    // budget" from "deep into grace" and decide how much unwinding it can
    // still afford.
    ImportDef { buf: BufSpec::None, ..imp!("budget_remaining_ns", Ctx, Sig { params: &[], results: &[I64] }, SYNC,
        "signed nanoseconds left against this dispatch's CPU budget; negative once the dispatch is \
         in its grace window and still falling, so a guest can tell `at budget` from `deep into \
         grace` and decide how much unwinding it can still afford") },
    // `unsafe_to_trap` is the bit the runtime layer publishes around GC phases
    // and scheduler queue mutations. The epoch callback **defers grace entry**
    // while it is set and the **hard deadline ignores it**: a pinned bit buys
    // exactly the grace window, nothing more.
    //
    // A production runtime layer would publish this as an exported mutable
    // global so the host reads it for free; an import costs one host call per
    // GC phase, which is the honest PoC shape and keeps the bit's owner
    // unambiguous.
    ImportDef { buf: BufSpec::None, ..imp!("unsafe_to_trap", Ctx, Sig { params: &[I32], results: &[] }, SYNC,
        "the runtime layer's unsafe-to-trap bit, set around GC phases and scheduler queue \
         mutations. Grace entry defers while it is set and the hard deadline ignores it, so a \
         pinned bit buys exactly the grace window and nothing more") },
];

pub fn lookup(name: &str) -> Option<&'static ImportDef> {
    IMPORTS.iter().find(|i| i.name == name)
}

/// Rule **V1**: an undeclared import is an attempted capability.
pub fn is_declared(module: &str, field: &str) -> bool {
    module == HOST_MODULE && lookup(field).is_some()
}

/// Imports marked for the baseline initialization policy.
pub fn init_visible() -> impl Iterator<Item = &'static ImportDef> {
    IMPORTS.iter().filter(|i| i.init_visible)
}

/// Imports marked as carrying sensitive data.
pub fn sensitive() -> impl Iterator<Item = &'static ImportDef> {
    IMPORTS.iter().filter(|i| i.sensitive)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A capability with no stated purpose is a capability nobody argued for.
    /// The macro makes the field mandatory; this is what stops `""`.
    #[test]
    fn every_import_says_what_it_is_for() {
        for i in IMPORTS {
            assert!(
                i.why.trim().len() > 20,
                "{}: `why` is missing or too short to be an explanation",
                i.name
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for i in IMPORTS {
            assert!(seen.insert(i.name), "duplicate import {}", i.name);
        }
    }


    /// The baseline initialization import set excludes these I/O groups.
    #[test]
    fn init_sandbox_exposes_no_io() {
        let io = [
            Group::Http,             Group::Clock, Group::Entropy,
        ];
        for i in init_visible() {
            assert!(!io.contains(&i.group), "init sandbox exposes I/O via {}", i.name);
        }
    }

    /// A `BufSpec` that disagrees with its signature would make the engine
    /// read a scalar as a pointer — i.e. hand the host an attacker-chosen
    /// offset into guest memory. Cheap to check, so check it.
    #[test]
    fn buffer_specs_agree_with_signatures() {
        for i in IMPORTS {
            let idx = match i.buf {
                BufSpec::None => continue,
                BufSpec::In(n) | BufSpec::Out(n) => n as usize,
            };
            assert!(
                idx + 1 < i.sig.params.len(),
                "{}: buffer at {} overruns a {}-param signature",
                i.name,
                idx,
                i.sig.params.len()
            );
            assert_eq!(
                (i.sig.params[idx], i.sig.params[idx + 1]),
                (Ty::I32, Ty::I32),
                "{}: (ptr, len) must be (i32, i32)",
                i.name
            );
        }
    }

    /// Requests bytes through the host entropy interface; captured guest values remain captured.
    #[test]
    fn host_writes_into_guest_memory_through_exactly_three_imports() {
        let out: Vec<_> = IMPORTS
            .iter()
            .filter(|i| matches!(i.buf, BufSpec::Out(_)))
            .map(|i| i.name)
            .collect();
        assert_eq!(out, vec!["init_hash_seed", "init_config_get", "entropy_fill"]);
    }

}
