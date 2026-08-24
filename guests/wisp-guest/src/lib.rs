//! Internal Rust guest-test support for capability bindings, async execution
//! and host preparation. Awaiting a host future returns control on an empty
//! guest stack; the host owns wake sources and resource lifetimes.

pub mod abi;
pub mod alloc;
pub mod app;
pub mod bindings;
pub mod budget;
pub mod ctx;
pub mod error;
pub mod future;
pub mod http;
pub mod init;
pub mod rt;

#[cfg(not(target_arch = "wasm32"))]
pub mod testing;

pub use app::{GcPressure, Handler};
pub use bindings::{
    fetch_bytes, log, BodyReader, Clock, Entropy, HttpFetch, Level, Span, };
pub use budget::{Budget, UnsafeToTrap};
pub use ctx::Ctx;
pub use error::{Result, WispError};
pub use future::{join2, join3, join_all, select2, Either, HostFuture};
pub use http::{Request, RequestExt, Response, ResponseStream};
pub use init::{InitUnit, LazyInit};

pub use wisp_abi;
pub use wisp_abi::result::{FutureError, };

/// Items the `export_*!` macros need to name. Not a stable surface.
#[doc(hidden)]
pub mod __rexport {
    pub use wisp_abi::dispatch::DispatchKind;
    pub use wisp_abi::status::TrapCode;

    pub fn into_handle(r: crate::Response) -> u32 {
        r.into_handle()
    }
}

/// §5.5 / §6.6. Called by the generated `wisp_gc_hint` export.
///
/// See [`alloc`] for exactly what this does; the short version is that it
/// rolls the bump pointer back over a free tail and zeroes everything
/// reclaimable, because zeroed pages compress out of the hibernation delta.
pub fn gc_hint(pressure: u8) {
    alloc::gc_hint(GcPressure::from_u8(pressure));
}

/// Current heap occupancy. Demo guests expose this so host-side delta tests
/// have a guest-visible number.
pub fn heap_stats() -> alloc::HeapStats {
    alloc::heap_stats()
}

/// Route Rust panics to the `abort` import so a fault carries a message, then
/// let the runtime trap.
///
/// §2.5.8: kills are abrupt and are never unwound. The release profile builds
/// with `panic = "abort"`, so a panic here is a wasm trap and the host
/// discards the instance -- there is no `finally`, no finalizer, and no
/// cleanup quantum, exactly as the tenant contract says.
pub fn install_panic_hook() {
    use core::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        let b = msg.as_bytes();
        unsafe { abi::abort(b.as_ptr() as i32, b.len() as i32) };
    }));
}

#[cfg(test)]
mod contract_tests {
    /// §2.5.1/§2.5.2 and §4.5: "no blocking" must be inexpressible, not merely
    /// discouraged. `wasm32-unknown-unknown` and rule V2 (threads/atomics off)
    /// carry most of that, but nothing stops an SDK author from writing a spin
    /// loop, so this greps the shipping sources for the shapes that would.
    ///
    /// Patterns are assembled from fragments so the test does not match its
    /// own source.
    #[test]
    fn the_sdk_contains_no_blocking_primitive() {
        let forbidden: &[&str] = &[
            concat!("std::th", "read"),
            concat!("th", "read::sleep"),
            concat!("Ins", "tant::now"),
            concat!("std::", "fs::"),
            concat!("std::", "net::"),
            concat!("blo", "ck_on"),
            concat!("spin", "_loop"),
            concat!("yield", "_now"),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // Host-only support code never reaches a wasm module.
            if p.file_name().unwrap() == "testing.rs" {
                continue;
            }
            let whole = std::fs::read_to_string(&p).unwrap();
            // Test modules are host-only and never reach a wasm module.
            let src = match whole.find("#[cfg(te") {
                Some(i) => &whole[..i],
                None => &whole[..],
            };
            checked += 1;
            for pat in forbidden {
                assert!(
                    !src.contains(pat),
                    "{} contains `{}`: the guest SDK must not be able to block (§2.5, §4.5)",
                    p.display(),
                    pat
                );
            }
        }
        assert!(checked >= 10, "expected to scan the whole SDK, saw {checked} files");
    }
}
