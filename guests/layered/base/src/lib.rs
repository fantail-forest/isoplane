//! The base layer — the buildable stand-in for `wisp:dotnet-runtime@<x>.<y>`.
//!
//! Everything a base template has to do, this module has to actually do:
//!
//! * **It owns the linear memory.** It is linked with
//!   `--initial-memory=8388608`, which does two things at once: it reserves
//!   the window `[4 MiB, 8 MiB)` that every app layer's static data and shadow
//!   stack are linked into (`--global-base=4194304`), and — because the arena
//!   in `wisp-rtlib::heap` starts the heap at `memory.size` as observed on
//!   first allocation — it puts the runtime heap at 8 MiB, above that window.
//!   `rt_heap_start()` reports it and the harness asserts the layout, so a
//!   collision is a failed run rather than silent corruption.
//! * **It exports the runtime ABI.** The wrappers below are generated from
//!   `wisp_rtabi::rt_surface_list!`, the same list the tenant side imports
//!   from, so the two halves cannot drift. They live in *this* crate rather
//!   than in `wisp-rtlib` because `#[no_mangle]` in a dependency rlib is not
//!   reliably retained — the same reason `guest-sdks/rust` is rlib-only.
//! * **It is un-trimmable, on purpose.** Every entry point is a root, so fat
//!   LTO cannot delete anything reachable from the full surface. That is
//!   the stated cost of layering — no trimming or devirtualization through the
//!   runtime, so the shared runtime ships un-trimmed — made literal, and it is
//!   the quantity the break-even analysis divides by.
//! * **It pre-initializes.** `wisp_initialize` is what wizer runs in the sealed
//!   init sandbox to produce the base template.
//!
//! **Rust, not .NET.** These fixtures model a split where almost the whole
//! module is shareable runtime. NativeAOT compiles the trimmed BCL into each
//! tenant module instead, so its shareable fraction is far smaller — do not
//! read a .NET break-even off these numbers.

#[global_allocator]
static ALLOC: wisp_rtlib::heap::RtAlloc = wisp_rtlib::heap::RtAlloc;

macro_rules! reexport {
    ($( fn $n:ident ( $($a:ident: $t:ty),* $(,)? ) -> $r:ty; )*) => {
        $(
            #[no_mangle]
            pub unsafe extern "C" fn $n($($a: $t),*) -> $r { wisp_rtabi::$n($($a),*) }
        )*
    };
}

wisp_rtabi::rt_surface_list!(reexport);

/// The init entry wizer runs in the sealed sandbox. Pre-initializing the base
/// is done **once per machine**, not once per tenant — that is the whole point
/// of layering, and it is why the cost of a big un-trimmed runtime is
/// amortized rather than multiplied.
#[no_mangle]
pub unsafe extern "C" fn wisp_initialize() {
    wisp_rtlib::abi::wisp_initialize()
}

/// The runtime layer's identity. Layering makes instance identity composite —
/// `(base hash, app hash)` — and hibernation deltas bind to the composite, so
/// this string moving is a fleet-wide wake-flush event.
#[no_mangle]
pub extern "C" fn rt_world_version() -> u32 {
    0x0001_0000
}
