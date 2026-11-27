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
    (module = $module:literal; $(
        $(#[$m:meta])*
        fn $name:ident ( $($p:ident : $pt:ident),* $(,)? ) $(-> $r:ident)? ;
    )*) => {
        // The real thing; the host linker defines these and nothing else. No
        // doc attributes here or on the items inside: rustdoc does not
        // document extern blocks and rustc warns about the attempt, so the
        // documentation lives on the host stubs below and on the import list
        // itself.
        #[cfg(target_arch = "wasm32")]
        #[link(wasm_import_module = $module)]
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

include!(concat!(env!("OUT_DIR"), "/imports.rs"));

#[cfg(test)]
mod tests {
    use isoplane_abi::imports::{self, Ty, IMPORTS};

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
}
