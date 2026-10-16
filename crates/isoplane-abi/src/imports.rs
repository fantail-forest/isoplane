//! Canonical guest capabilities consumed by the validator, linker and SDK.
//! This is the shared definition used to check declared imports and signatures.

/// Core-wasm module name that every Isoplane import lives under.
pub const HOST_MODULE: &str = "isoplane";

/// Clock and entropy capability names considered during local preparation.
pub const RUNTIME_INIT_CAPABILITIES: &[&str] = &["clock_now_coarse", "entropy_fill"];

pub use crate::exports;

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
    None,
    /// Guest to host: `(params[i], params[i+1])` is `(ptr, len)`, read out of
    /// guest memory for the duration of the call.
    In(u8),
    /// Host to guest: `(params[i], params[i+1])` is `(ptr, cap)`; the host
    /// fills a scratch buffer that the engine copies into guest memory.
    Out(u8),
}

/// The most an out-buffer import may ask the host to produce in one call.
/// Application budgets cap the guest's own memory; this caps what the guest can make
/// the host allocate on its behalf.
pub const MAX_OUT_BUFFER_BYTES: usize = 1 << 20;

/// The least interval `timers_between_requests` accepts.
pub const MIN_TIMERS_BETWEEN_REQUESTS_MS: u64 = 100;

/// Maximum encoded activation configuration size, bounded by the output-buffer limit.
pub const MAX_RUNTIME_CONFIGURATION_BYTES: usize = MAX_OUT_BUFFER_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Sync,
    /// Returns a future id immediately. No import waits.
    Future,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Core,
    Observe,
    Clock,
    Entropy,
    Http,
    Response,
    Socket,
    Ctx,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportDef {
    pub name: &'static str,
    pub group: Group,
    pub sig: Sig,
    pub kind: ImportKind,
    pub buf: BufSpec,
    /// Marks values that require sensitive-data handling.
    pub sensitive: bool,
    /// Included in the baseline initialization import set.
    pub init_visible: bool,
    /// What the capability is for, for a tenant reading a manifest. Required
    /// by `imp!` so a capability cannot be added without saying.
    pub why: &'static str,
}

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

const BUF_TO_FUTURE: Sig = Sig {
    params: &[I32, I32],
    results: &[I64],
};
const BUF_TO_VOID: Sig = Sig {
    params: &[I32, I32],
    results: &[],
};
const BUF_TO_I32: Sig = Sig {
    params: &[I32, I32],
    results: &[I32],
};

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

pub const IMPORTS: &[ImportDef] = &[
    ImportDef { buf: BufSpec::None, ..imp!("socket_call", Socket, Sig { params: &[I32, I32, I32], results: &[I64] }, SYNC,
        "instance-owned outbound TCP operations; canonical scalar packet; connections are authorized by host egress policy") },
    ImportDef { buf: BufSpec::None, ..imp!("poll_call", Clock, Sig { params: &[I32, I32, I32], results: &[I64] }, SYNC,
        "nonblocking resource readiness and coarse deadlines; pending work wakes through host-owned futures") },
    ImportDef { init_visible: true, buf: BufSpec::None,
        ..imp!("filesystem_call", Core, Sig { params: &[I32, I32, I32], results: &[] }, SYNC,
        "read-only filesystem operations over the host-declared platform files; arguments are canonical scalars in a guest u64 array; all filesystem records remain in guest memory") },
    // ---- core -------------------------------------------------------------
    ImportDef { init_visible: true, buf: BufSpec::None,
        ..imp!("memory_limit_bytes", Core, Sig { params: &[], results: &[I64] }, SYNC,
        "effective total linear-memory ceiling in bytes, including the module maximum; available before runtime initialization so guest collectors observe the enforced budget") },
    ImportDef { init_visible: true, ..imp!("abort", Core, BUF_TO_VOID, SYNC,
        "guest panic; available at init so a failed pre-initialization reports usefully rather than \
         trapping anonymously") },
    // The guest announces each init unit before running it; when a unit traps
    // on a forbidden import, pre-init is replayed with that unit deferred to
    // ordinary lazy execution.
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
    ImportDef {
        init_visible: true,
        sensitive: true,
        ..ImportDef { buf: BufSpec::Out(0), ..imp!("init_hash_seed", Core, BUF_TO_VOID, SYNC,
            "A per-attempt string-hash seed, separate from host entropy requests.") }
    },

    // Tenant-authored deploy input, fixed for the version.
    ImportDef {
        init_visible: true,
        ..ImportDef { buf: BufSpec::Out(0), ..imp!("init_config_get", Core, BUF_TO_I32, SYNC,
            "the init-visible config blob, so a config parse happens once at deploy time instead of \
             once per activation; returns the blob's FULL length, so a guest can size its buffer \
             with a zero-length probe") }
    },

    ImportDef {
        sensitive: true,
        init_visible: true,
        ..ImportDef { buf: BufSpec::Out(0), ..imp!("runtime_config_get", Core, BUF_TO_I32, SYNC,
            "deployment-bound JSON configuration from the application owner; returns the full \
             byte length for a zero-length sizing probe; available during preparation and activation") }
    },

    // ---- observability ----------------------------------------------------
    ImportDef { buf: BufSpec::In(1), init_visible: true, ..imp!("log_emit", Observe, Sig { params: &[I32, I32, I32], results: &[] }, SYNC,
        "structured log at `trace=0` .. `error=4`; worker occupancy counts against the dispatch budget, \
         which is what stops log-as-compute abuse") },
    ImportDef { buf: BufSpec::None, ..imp!("development_notices", Observe, Sig { params: &[], results: &[I64] }, SYNC,
        "non-zero where the host shows guests' development notices, such as what their timers will cost: in local \
         development and tests, never in a deployment") },
    imp!("trace_span_begin", Observe, BUF_TO_I32, SYNC,
        "opens a trace span and returns its id, for correlating work inside one dispatch"),
    ImportDef { buf: BufSpec::None, ..imp!("trace_span_end", Observe, Sig { params: &[I32], results: &[] }, SYNC,
        "closes the span with that id; unbalanced spans are the guest's problem, not the host's") },

    // ---- clock & entropy --------------------------------------------------
    // Not init-visible: a static initialiser that reads the clock is impure.
    ImportDef { buf: BufSpec::None, ..imp!("clock_now_coarse", Clock, Sig { params: &[], results: &[I64] }, SYNC,
        "Reads the host coarse clock; use host deadline futures to wait.") },
    ImportDef { init_visible: true, buf: BufSpec::None,
        ..imp!("timers_between_requests", Core, Sig { params: &[I64], results: &[I64] }, SYNC,
        "opts the application's instances in to being woken while idle to run due timers, at most once per the given \
         number of milliseconds, which is at least MIN_TIMERS_BETWEEN_REQUESTS_MS; \
         only during initialization, and non-zero after it") },
    ImportDef { buf: BufSpec::None, ..imp!("clock_deadline_in", Clock, Sig { params: &[I64], results: &[I64] }, FUT,
        "a timer as a host future, so waiting is never a guest loop burning budget") },
    ImportDef { buf: BufSpec::Out(0), ..imp!("entropy_fill", Entropy, BUF_TO_VOID, SYNC,
        "Host CSPRNG bytes for this request; values already stored in guest memory remain part of captured state.") },

    // ---- outbound http ----------------------------------------------------
    imp!("http_fetch", Http, BUF_TO_FUTURE, FUT,
        "Issues an outbound request through the host egress policy and completes with its response head."),
    ImportDef { buf: BufSpec::None, ..imp!("http_body_read", Http, Sig { params: &[I32, I32], results: &[I64] }, FUT,
        "Reads the next chunk of a response body through a host-owned future.") },

    // ---- inbound request --------------------------------------------------
    ImportDef { buf: BufSpec::None, ..imp!("req_body_read", Http, Sig { params: &[I32], results: &[I64] }, FUT,
        "reads at most max bytes from this dispatch's upload; max is positive, empty bytes mean EOF; declaring this capability permits inline body bytes followed by a host-owned upload") },
    ImportDef { buf: BufSpec::None, ..imp!("req_body_can_have_body", Http, Sig { params: &[], results: &[I32] }, SYNC,
        "whether HTTP framing permits content for this dispatch, including unknown-length uploads") },

    // ---- inbound response -------------------------------------------------
    imp!("resp_new", Response, BUF_TO_I32, SYNC,
        "opens a response from a `ResponseHead` and returns its handle"),
    ImportDef { buf: BufSpec::In(1), ..imp!("resp_write", Response, Sig { params: &[I32, I32, I32], results: &[I64] }, FUT,
        "appends a streamed chunk to an open response") },
    ImportDef { buf: BufSpec::None, ..imp!("resp_finish", Response, Sig { params: &[I32], results: &[] }, SYNC,
        "ends the response, after which the handle is spent") },


    // ---- the kill-cooperation ABI ------------------------------------------
    // Engine-owned rather than in the bindings vtable: the bit lives in the
    // engine's store, beside the meter the epoch callback compares against.
    // There is deliberately no import that reads the budget. SECURITY: a value
    // that falls while the guest runs is a clock, and side-channel attacks
    // need exactly that; a guest learns its budget ran out through
    // cancellation at its next slice.
    ImportDef { buf: BufSpec::None, ..imp!("unsafe_to_trap", Ctx, Sig { params: &[I32], results: &[] }, SYNC,
        "the runtime layer's unsafe-to-trap bit, set around GC phases and scheduler queue \
         mutations. Grace entry defers while it is set and the hard deadline ignores it, so a \
         pinned bit buys exactly the grace window and nothing more") },
];

pub fn lookup(name: &str) -> Option<&'static ImportDef> {
    IMPORTS.iter().find(|i| i.name == name)
}

pub fn is_declared(module: &str, field: &str) -> bool {
    module == HOST_MODULE && lookup(field).is_some()
}

pub fn init_visible() -> impl Iterator<Item = &'static ImportDef> {
    IMPORTS.iter().filter(|i| i.init_visible)
}


#[cfg(test)]
mod tests {
    use super::*;

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
            Group::Http,
            Group::Socket,
            Group::Clock,
            Group::Entropy,
        ];
        for i in init_visible() {
            assert!(
                !io.contains(&i.group),
                "init sandbox exposes I/O via {}",
                i.name
            );
        }
    }

    /// A `BufSpec` that disagrees with its signature would have the engine
    /// read a scalar as a pointer into guest memory.
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

    /// An additional out-buffer import is a new path for host bytes to cross the
    /// sandbox boundary, and gets a design conversation before this list.
    #[test]
    fn host_writes_into_guest_memory_through_the_reviewed_imports() {
        let out: Vec<_> = IMPORTS
            .iter()
            .filter(|i| matches!(i.buf, BufSpec::Out(_)))
            .map(|i| i.name)
            .collect();
        assert_eq!(
            out,
            vec![
                "init_hash_seed",
                "init_config_get",
                "runtime_config_get",
                "entropy_fill"
            ],
            "the set of imports that write into guest memory has changed"
        );
    }

}
