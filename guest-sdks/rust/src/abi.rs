//! The raw import surface.
//!
//! The SDK's capabilities are a subset of [`isoplane_abi::imports::IMPORTS`],
//! under module `"isoplane"` with the canonical signatures. Nothing else is declared, because
//! the import list *is* the capability manifest: the validator rejects a module
//! that imports anything else at deploy time.
//!
//! The [`isoplane_imports!`] macro emits three things from one list:
//!
//! * the `unsafe extern "C"` block, on `wasm32` only;
//! * host stubs, so the SDK's own tests can run on the development machine;
//! * [`SDK_IMPORTS`], a descriptor table which
//!   [`tests::sdk_matches_the_contract`] checks against the canonical names
//!   and signatures. Drift is a compile-and-test
//!   failure rather than a mysterious instantiation error in production.

use isoplane_abi::imports::{Sig, Ty};

/// One entry of [`SDK_IMPORTS`]: the SDK's own view of an import's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkImport {
    pub name: &'static str,
    pub sig: Sig,
}

macro_rules! wty {
    (i32) => {
        Ty::I32
    };
    (i64) => {
        Ty::I64
    };
}
macro_rules! wres {
    () => {
        &[]
    };
    ($t:ident) => {
        &[wty!($t)]
    };
}

#[cfg(not(target_arch = "wasm32"))]
macro_rules! wstub_body {
    ($name:ident, [$($p:ident),*],) => {{
        crate::abi::host_stub_hook(stringify!($name), &[$($p as i64),*]);
    }};
    ($name:ident, [$($p:ident),*], $r:ident) => {{
        crate::abi::host_stub_hook(stringify!($name), &[$($p as i64),*]) as $r
    }};
}

macro_rules! isoplane_imports {
    ($(
        $(#[$m:meta])*
        fn $name:ident ( $($p:ident : $pt:ident),* $(,)? ) $(-> $r:ident)? ;
    )*) => {
        // The real thing. `wasm_import_module = "isoplane"` is
        // `isoplane_abi::imports::HOST_MODULE`; the host linker defines these and
        // nothing else. No doc attributes here or on the items inside:
        // rustdoc does not document extern blocks and rustc warns about the
        // attempt, so the documentation lives on the host stubs below and on
        // the import list itself.
        #[cfg(target_arch = "wasm32")]
        #[link(wasm_import_module = "isoplane")]
        unsafe extern "C" {
            $( pub fn $name($($p: $pt),*) $(-> $r)?; )*
        }

        // Host builds: the same names with the same signatures so the SDK
        // type-checks and its non-wasm unit tests link. A guest never reaches
        // these -- they exist so the executor's park-without-spinning
        // behaviour can be tested with `cargo test` on a laptop.
        #[cfg(not(target_arch = "wasm32"))]
        #[allow(unused_variables, dead_code, clippy::missing_safety_doc)]
        pub mod stubs {
            $( $(#[$m])*
               pub unsafe extern "C" fn $name($($p: $pt),*) $(-> $r)? {
                   wstub_body!($name, [$($p),*], $($r)?)
               }
            )*
        }
        #[cfg(not(target_arch = "wasm32"))]
        pub use stubs::*;

        /// What this SDK believes the import table is. Checked against the
        /// contract in tests.
        pub const SDK_IMPORTS: &[SdkImport] = &[$(
            SdkImport {
                name: stringify!($name),
                sig: Sig { params: &[$(wty!($pt)),*], results: wres!($($r)?) },
            },
        )*];
    };
}

/// Read a `BufSpec::Out(0)` import that reports the blob's full length --
/// `(ptr, cap) -> full_len`, the probe-then-fill idiom.
///
/// On wasm this is the import call and nothing else, which is the path that
/// ships. On host builds a guest pointer truncates to `i32`, so the stub hands
/// the bytes back through [`crate::testing`] instead; that shim exists only so
/// the idiom itself is testable without a wasm engine.
#[cfg(target_arch = "wasm32")]
pub(crate) fn read_sized_out(imp: unsafe extern "C" fn(i32, i32) -> i32, dst: &mut [u8]) -> i32 {
    unsafe { imp(dst.as_mut_ptr() as i32, dst.len() as i32) }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_sized_out(imp: unsafe extern "C" fn(i32, i32) -> i32, dst: &mut [u8]) -> i32 {
    let full = unsafe { imp(0, dst.len() as i32) };
    crate::testing::take_out_buffer(dst);
    full
}

/// Host-build stub behaviour. Tests install a hook so they can drive the
/// executor without a wasm engine; unhooked calls return 0.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn host_stub_hook(name: &'static str, args: &[i64]) -> i64 {
    crate::testing::stub_call(name, args)
}

isoplane_imports! {
    // ---- core -------------------------------------------------------------
    /// Guest panic. `init_visible`, so a failed pre-init reports usefully.
    fn abort(ptr: i32, len: i32);
    /// Impure-cctor deferral: announce the unit id *before* running it, so
    /// that if the unit traps on a forbidden import the host knows exactly
    /// which one to defer when it replays pre-initialization.
    fn init_unit_begin(id: i32);
    /// Non-zero => the host has already decided this unit is impure. Skip it;
    /// it runs lazily, per instance, on first touch.
    fn init_unit_deferred(id: i32) -> i32;
    /// The preparation string-hash seed is separate from entropy requests and can be captured in guest state.
    fn init_hash_seed(ptr: i32, len: i32);
    /// Copies preparation configuration into the guest buffer and returns its full length.
    fn init_config_get(ptr: i32, cap: i32) -> i32;

    // ---- observability ----------------------------------------------------
    /// `(level, ptr, len)`. Counts against the dispatch CPU budget, which is
    /// what stops log-as-compute abuse.
    fn log_emit(level: i32, ptr: i32, len: i32);
    fn trace_span_begin(ptr: i32, len: i32) -> i32;
    fn trace_span_end(span: i32);

    /// The serving slice's fixed coarse time, in milliseconds since the Unix epoch.
    fn clock_now_coarse() -> i64;
    /// Creates a host timer future that completes after the requested delay.
    fn clock_deadline_in(ms: i64) -> i64;
    /// Requests bytes through the host entropy interface; captured guest values remain captured.
    fn entropy_fill(ptr: i32, len: i32);

    // ---- outbound HTTP ---------------------------------------------------
    fn http_fetch(ptr: i32, len: i32) -> i64;
    fn http_body_read(handle: i32, max: i32) -> i64;

    // ---- inbound response -------------------------------------------------
    fn resp_new(ptr: i32, len: i32) -> i32;
    fn resp_write(handle: i32, ptr: i32, len: i32) -> i64;
    fn resp_finish(handle: i32);





    // ---- the kill-cooperation ABI ------------------------------------------
    /// The host-published remaining-budget value: signed nanoseconds left
    /// against this dispatch's budget B. Negative once the dispatch is in its
    /// grace window, and falling.
    ///
    /// The .NET runtime layer reads this at the allocation slow path, GC entry
    /// and task switch, and raises `BudgetExceededException` when it is spent.
    /// This SDK reads it at its own equivalent boundaries — see
    /// [`crate::budget`].
    ///
    /// It is not a clock. It advances only with this guest's own thread CPU, so
    /// differencing it measures the guest measuring itself and gives back no
    /// fine-grained wall-clock timer.
    fn budget_remaining_ns() -> i64;
    /// The runtime layer's *unsafe-to-trap* bit: non-zero while a GC phase or a
    /// scheduler queue mutation is in progress. Grace entry defers while it is
    /// set; the hard deadline at B+G ignores it.
    fn unsafe_to_trap(on: i32);
}

// ---------------------------------------------------------------------------
// The guest exports.
//
// `isoplane_abi::imports::exports::SIGNATURES` is the contract (it was not when
// this SDK was first written -- see guests/README.md gap G1, resolved). The
// table below is what the `export_*!` macros actually emit, kept separate so
// that `exports_match_the_contract` can catch drift rather than have the two
// definitions be the same line of code.
// ---------------------------------------------------------------------------

/// One guest export, as this SDK lowers it to core wasm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkExport {
    pub name: &'static str,
    pub sig: Sig,
}

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// The lowering of the WIT world's guest exports onto a flat core module.
/// Host-side buffers are handed over by `isoplane_alloc` + copy; every entrypoint
/// returns a packed [`isoplane_abi::PollStatus`].
pub const SDK_EXPORTS: &[SdkExport] = &[
    // isoplane-alloc: func(size: u32, align: u32) -> u32
    SdkExport {
        name: "isoplane_alloc",
        sig: Sig {
            params: &[I32, I32],
            results: &[I32],
        },
    },
    // isoplane-handle: func(dispatch: dispatch-record) -> poll-status
    // ...lowered as (ptr, len) of a postcard `DispatchRecord` in a buffer the
    // host obtained from `isoplane_alloc`. The guest frees it.
    SdkExport {
        name: "isoplane_handle",
        sig: Sig {
            params: &[I32, I32],
            results: &[I64],
        },
    },
    // isoplane-wake: func(future: u64, result: result-record) -> poll-status
    SdkExport {
        name: "isoplane_wake",
        sig: Sig {
            params: &[I64, I32, I32],
            results: &[I64],
        },
    },
    // isoplane-gc-hint: func(pressure: u8)
    SdkExport {
        name: "isoplane_gc_hint",
        sig: Sig {
            params: &[I32],
            results: &[],
        },
    },
    // Pre-init entrypoint, run once in the sealed init sandbox.
    SdkExport {
        name: "isoplane_initialize",
        sig: Sig {
            params: &[],
            results: &[I64],
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_abi::imports::{self, BufSpec, IMPORTS};

    /// Every capability this SDK declares must link against the canonical host
    /// interface; capabilities the SDK does not expose need no declaration.
    #[test]
    fn sdk_matches_the_contract() {
        for s in SDK_IMPORTS {
            let a = imports::lookup(s.name).expect("declared but not in the contract");
            assert_eq!(s.sig, a.sig, "signature drift on `{}`", s.name);
        }
    }

    /// "Suspension implies an empty stack" is structural, but it is worth
    /// asserting that nothing in the surface we declare *could* wait: every
    /// non-`Sync` import hands back a future id and returns.
    #[test]
    fn no_declared_import_can_block() {
        for i in IMPORTS {
            if let imports::ImportKind::Future = i.kind {
                assert_eq!(
                    i.sig.results,
                    &[Ty::I64],
                    "{} is future-returning but does not return a future id",
                    i.name
                );
            }
        }
    }

    /// The export half of `sdk_matches_the_contract`. `exports::SIGNATURES`
    /// is what the engine's `get_typed_func` is written against, so a
    /// disagreement here is an instantiation failure at activation time.
    #[test]
    fn exports_match_the_contract() {
        for e in SDK_EXPORTS {
            let want = imports::exports::signature(e.name)
                .unwrap_or_else(|| panic!("{} is not a declared export", e.name));
            assert_eq!(e.sig, want, "export signature drift on `{}`", e.name);
        }
    }

    /// `BufSpec` resolves an ambiguity the raw signature cannot: which
    /// `(i32, i32)` pairs are buffers, which way the bytes flow, and where the
    /// pair sits among the scalars. The SDK hard-codes that at each call site,
    /// so this asserts the hard-coding still matches the contract. Getting it
    /// wrong would hand the host a length as a pointer.
    #[test]
    fn buffer_layout_matches_every_call_site() {
        // What `crate::bindings` and `crate::init` assume, written out.
        let assumed: &[(&str, BufSpec)] = &[
            // Host writes INTO guest memory: we pass (ptr, capacity) of a
            // buffer we own. Only these two.
            ("entropy_fill", BufSpec::Out(0)),
            ("init_hash_seed", BufSpec::Out(0)),
            ("init_config_get", BufSpec::Out(0)),
            // A scalar before the pair.
            ("log_emit", BufSpec::In(1)),
            ("resp_write", BufSpec::In(1)),
            // A scalar after the pair.
            // Looks like a buffer import, is not: (handle, max_len).
            ("http_body_read", BufSpec::None),
            ("clock_deadline_in", BufSpec::None),
            ("budget_remaining_ns", BufSpec::None),
            ("unsafe_to_trap", BufSpec::None),
            ("resp_finish", BufSpec::None),
            ("trace_span_end", BufSpec::None),
            ("init_unit_begin", BufSpec::None),
            ("init_unit_deferred", BufSpec::None),
            // Plain postcard payloads.
            ("abort", BufSpec::In(0)),
            ("http_fetch", BufSpec::In(0)),
            ("resp_new", BufSpec::In(0)),
        ];
        for (name, want) in assumed {
            let got = imports::lookup(name).unwrap().buf;
            assert_eq!(got, *want, "BufSpec drift on `{name}`");
        }
        // And the SDK's blanket assumption: nothing else is an out-buffer.
        let outs: Vec<&str> = IMPORTS
            .iter()
            .filter(|i| matches!(i.buf, BufSpec::Out(_)))
            .map(|i| i.name)
            .collect();
        assert_eq!(
            outs,
            vec!["init_hash_seed", "init_config_get", "entropy_fill"],
            "a new host-writes-into-guest-memory import needs an SDK call site"
        );
    }

    #[test]
    fn exports_cover_the_required_set() {
        for name in imports::exports::REQUIRED {
            if *name == imports::exports::MEMORY {
                continue; // emitted by the linker, not by us
            }
            assert!(
                SDK_EXPORTS.iter().any(|e| e.name == *name),
                "missing export {name}"
            );
        }
    }
}
