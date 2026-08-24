//! The raw import surface (spec.md §5.3, invariant 4).
//!
//! Exactly the functions in [`wisp_abi::imports::IMPORTS`], under module
//! `"wisp"`, with exactly those signatures. Nothing else is declared, because
//! an undeclared import is an attempted capability and the validator rejects
//! the module at deploy time (rule V1, §4.3).
//!
//! The [`wisp_imports!`] macro emits three things from one list:
//!
//! * the `unsafe extern "C"` block, on `wasm32` only;
//! * host stubs, so the SDK's own tests can run on the development machine;
//! * [`SDK_IMPORTS`], a descriptor table which
//!   [`tests::sdk_matches_the_contract`] compares element-for-element against
//!   `wisp_abi::imports::IMPORTS`. Drift between the two is a compile-and-test
//!   failure rather than a mysterious instantiation error in production.

use wisp_abi::imports::{Sig, Ty};

/// One entry of [`SDK_IMPORTS`]: the SDK's own view of an import's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkImport {
    pub name: &'static str,
    pub sig: Sig,
}

macro_rules! wty {
    (i32) => { Ty::I32 };
    (i64) => { Ty::I64 };
}
macro_rules! wres {
    () => { &[] };
    ($t:ident) => { &[wty!($t)] };
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

macro_rules! wisp_imports {
    ($(
        $(#[$m:meta])*
        fn $name:ident ( $($p:ident : $pt:ident),* $(,)? ) $(-> $r:ident)? ;
    )*) => {
        // The real thing. `wasm_import_module = "wisp"` is
        // `wisp_abi::imports::HOST_MODULE`; the host linker defines these and
        // nothing else. No doc attributes here or on the items inside:
        // rustdoc does not document extern blocks and rustc warns about the
        // attempt, so the documentation lives on the host stubs below and on
        // the import list itself.
        #[cfg(target_arch = "wasm32")]
        #[link(wasm_import_module = "wisp")]
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

/// Host-build stub behaviour. Tests install a hook so they can drive the
/// executor without a wasm engine; unhooked calls return 0.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn host_stub_hook(name: &'static str, args: &[i64]) -> i64 {
    crate::testing::stub_call(name, args)
}

wisp_imports! {
    // ---- core -------------------------------------------------------------
    /// Guest panic. `init_visible`, so a failed pre-init reports usefully.
    fn abort(ptr: i32, len: i32);
    /// §4.4 impure-cctor deferral: announce the unit id *before* running it,
    /// so that if the unit traps on a forbidden import the host knows exactly
    /// which one to defer when it replays pre-initialization.
    fn init_unit_begin(id: i32);
    /// Non-zero => the host has already decided this unit is impure. Skip it;
    /// it runs lazily, per instance, on first touch.
    fn init_unit_deferred(id: i32) -> i32;
    /// The preparation string-hash seed is separate from entropy requests and can be captured in guest state.
    fn init_hash_seed(ptr: i32, len: i32);

    // ---- observability ----------------------------------------------------
    /// `(level, ptr, len)`. Counts against the dispatch CPU budget, which is
    /// what stops log-as-compute abuse (§10).
    fn log_emit(level: i32, ptr: i32, len: i32);
    fn trace_span_begin(ptr: i32, len: i32) -> i32;
    fn trace_span_end(span: i32);

    // ---- clock & entropy --------------------------------------------------
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





}

// ---------------------------------------------------------------------------
// The guest exports (§5.2).
//
// CONTRACT GAP: `wisp_abi::imports::exports` names the five exports but does
// not give their core-wasm signatures, so the engine and the SDK could
// disagree silently. This is the lowering the SDK emits; see guests/README.md.
// ---------------------------------------------------------------------------

/// One guest export, as this SDK lowers it to core wasm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkExport {
    pub name: &'static str,
    pub sig: Sig,
}

const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;

/// The lowering of the §5.2 WIT world onto a flat core module. Host-side
/// buffers are handed over by `wisp_alloc` + copy; every entrypoint returns a
/// packed [`wisp_abi::PollStatus`].
pub const SDK_EXPORTS: &[SdkExport] = &[
    // wisp-alloc: func(size: u32, align: u32) -> u32
    SdkExport { name: "wisp_alloc", sig: Sig { params: &[I32, I32], results: &[I32] } },
    // wisp-handle: func(dispatch: dispatch-record) -> poll-status
    // ...lowered as (ptr, len) of a postcard `DispatchRecord` in a buffer the
    // host obtained from `wisp_alloc`. The guest frees it.
    SdkExport { name: "wisp_handle", sig: Sig { params: &[I32, I32], results: &[I64] } },
    // wisp-wake: func(future: u64, result: result-record) -> poll-status
    SdkExport { name: "wisp_wake", sig: Sig { params: &[I64, I32, I32], results: &[I64] } },
    // wisp-gc-hint: func(pressure: u8)
    SdkExport { name: "wisp_gc_hint", sig: Sig { params: &[I32], results: &[] } },
    // Pre-init entrypoint, run once in the sealed init sandbox (§4.4).
    SdkExport { name: "wisp_initialize", sig: Sig { params: &[], results: &[] } },
];

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::imports::{self, IMPORTS};

    /// The one test that keeps three consumers of one table honest: the
    /// validator, the host linker and this SDK. If `wisp-abi` grows an import
    /// and the SDK does not declare it, this fails; if the SDK declares
    /// something `wisp-abi` does not, V1 would reject every module we build,
    /// and this fails first.
    #[test]
    fn sdk_matches_the_contract() {
        let sdk: Vec<&str> = SDK_IMPORTS.iter().map(|i| i.name).collect();
        let abi: Vec<&str> = IMPORTS.iter().map(|i| i.name).collect();
        assert_eq!(sdk, abi, "import name/order drift between wisp-guest and wisp-abi");

        for s in SDK_IMPORTS {
            let a = imports::lookup(s.name).expect("declared but not in the contract");
            assert_eq!(s.sig, a.sig, "signature drift on `{}`", s.name);
        }
        assert_eq!(SDK_IMPORTS.len(), IMPORTS.len());
    }

    /// Invariant 3 is structural, but it is worth asserting that nothing in
    /// the surface we declare *could* wait: every non-`Sync` import hands back
    /// a future id and returns.
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

    #[test]
    fn exports_cover_the_required_set() {
        for name in imports::exports::REQUIRED {
            if *name == imports::exports::MEMORY {
                continue; // emitted by the linker, not by us
            }
            assert!(SDK_EXPORTS.iter().any(|e| e.name == *name), "missing export {name}");
        }
        for name in imports::exports::OPTIONAL {
            assert!(SDK_EXPORTS.iter().any(|e| e.name == *name), "missing export {name}");
        }
    }
}
