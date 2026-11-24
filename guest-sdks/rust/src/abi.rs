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
    /// Guest panic. `init_visible`, so a failed pre-init reports usefully.
    fn abort(ptr: i32, len: i32);
    /// The deployment's settings as a JSON object of string values. The host
    /// writes up to `cap` bytes at `ptr` and returns the full length, so the
    /// idiom is a zero-length probe then a second call. Values can be secret,
    /// and what initialization keeps of them is captured in the template.
    fn runtime_config_get(ptr: i32, cap: i32) -> i32;

    /// `(level, ptr, len)`. Counts against the dispatch occupancy budget, which is
    /// what stops log-as-compute abuse.
    fn log_emit(level: i32, ptr: i32, len: i32);
    fn trace_span_begin(ptr: i32, len: i32) -> i32;
    fn trace_span_end(span: i32);

    /// The serving slice's fixed coarse time, in milliseconds since the Unix epoch.
    fn clock_now_coarse() -> i64;
    /// Returns a host timer future; awaiting it returns control to the host.
    fn clock_deadline_in(ms: i64) -> i64;
    /// Requests bytes through the host entropy interface; captured guest values remain captured.
    fn entropy_fill(ptr: i32, len: i32);

    fn http_fetch(ptr: i32, len: i32) -> i64;
    /// Read at most `max` bytes from the response body handle.
    fn http_body_read(handle: i32, max: i32) -> i64;

    fn resp_new(ptr: i32, len: i32) -> i32;
    fn resp_write(handle: i32, ptr: i32, len: i32) -> i64;
    fn resp_finish(handle: i32);

    /// The runtime layer's *unsafe-to-trap* bit: non-zero while a GC phase or a
    /// scheduler queue mutation is in progress. Grace entry defers while it is
    /// set; the hard deadline at B+G ignores it.
    fn unsafe_to_trap(on: i32);
}

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
/// returns a packed [`isoplane_abi::PollStatus`]. Kept separate from
/// `isoplane_abi::imports::exports::SIGNATURES`, the contract, so that
/// `exports_match_the_contract` catches drift in what the `export_*!` macros emit.
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
    // Pre-init entrypoint, run once in the init sandbox.
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

    #[test]
    fn sdk_matches_the_contract() {
        for s in SDK_IMPORTS {
            let a = imports::lookup(s.name).expect("declared but not in the contract");
            assert_eq!(s.sig, a.sig, "signature drift on `{}`", s.name);
        }
    }

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

    /// `exports::SIGNATURES` is what the engine's `get_typed_func` is written
    /// against, so a disagreement here is an instantiation failure at
    /// activation time.
    #[test]
    fn exports_match_the_contract() {
        for e in SDK_EXPORTS {
            let want = imports::exports::signature(e.name)
                .unwrap_or_else(|| panic!("{} is not a declared export", e.name));
            assert_eq!(e.sig, want, "export signature drift on `{}`", e.name);
        }
    }

    /// The SDK hard-codes the buffer layout at each call site; getting it
    /// wrong would hand the host a length as a pointer.
    #[test]
    fn buffer_layout_matches_every_call_site() {
        // What `crate::bindings` and `crate::init` assume, written out.
        let assumed: &[(&str, BufSpec)] = &[
            // Host writes INTO guest memory: we pass (ptr, capacity) of a
            // buffer we own.
            ("entropy_fill", BufSpec::Out(0)),
            ("runtime_config_get", BufSpec::Out(0)),
            // A scalar before the pair.
            ("log_emit", BufSpec::In(1)),
            ("resp_write", BufSpec::In(1)),
            // A scalar after the pair.
            // Looks like a buffer import, is not: (handle, max_len).
            ("http_body_read", BufSpec::None),
            ("clock_deadline_in", BufSpec::None),
            ("unsafe_to_trap", BufSpec::None),
            ("resp_finish", BufSpec::None),
            ("trace_span_end", BufSpec::None),
            // Plain postcard payloads.
            ("abort", BufSpec::In(0)),
            ("http_fetch", BufSpec::In(0)),
            ("resp_new", BufSpec::In(0)),
        ];
        for (name, want) in assumed {
            let got = imports::lookup(name).unwrap().buf;
            assert_eq!(got, *want, "BufSpec drift on `{name}`");
        }
        // Every out-buffer binding exposed by this SDK needs a checked call site.
        let outs: Vec<&str> = IMPORTS
            .iter()
            .filter(|i| {
                matches!(i.buf, BufSpec::Out(_))
                    && SDK_IMPORTS.iter().any(|binding| binding.name == i.name)
            })
            .map(|i| i.name)
            .collect();
        assert_eq!(
            outs,
            vec!["runtime_config_get", "entropy_fill"],
            "an SDK out-buffer binding needs a checked call site"
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
