//! Tenant 4: hello world. Reaches nothing but the dispatch codec — the floor case, and the one a layered runtime needs cheap if new tenants are to be admitted quickly.
//!
//! Written once, built twice: `--features layered` compiles it against the
//! shared runtime layer's imported ABI (the module then contains app code and
//! nothing else), `--features selfcontained` links the identical runtime code
//! in and lets fat LTO trim it to this app's reachable subset. The two
//! artifacts are what `bench/dotnet-sharing`'s `layered` subcommand compares.

#![cfg_attr(feature = "layered", no_std)]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::vec::Vec;
use wisp_rtabi::*;

#[global_allocator]
static ALLOC: wisp_rtabi::RtAlloc = wisp_rtabi::RtAlloc;

#[cfg(feature = "layered")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// Copy the runtime layer's out channel, but only if the call succeeded.
unsafe fn take_out_after(n: i32) -> Vec<u8> {
    if n < 0 { Vec::new() } else { take_out() }
}

#[allow(dead_code)]
unsafe fn rt_str(n: i32) -> alloc::string::String {
    alloc::string::String::from_utf8(take_out_after(n)).unwrap_or_default()
}

/// Host→guest buffer handoff. In `layered` mode this hands back a pointer
/// into the *runtime layer's* heap: the app owns no memory of its own.
#[no_mangle]
pub unsafe extern "C" fn wisp_alloc(size: u32, align: u32) -> *mut u8 {
    rt_alloc(size, align)
}

#[no_mangle]
pub unsafe extern "C" fn wisp_handle(ptr: *const u8, len: u32) -> u64 {
    let d = core::slice::from_raw_parts(ptr, len as usize);

    let path = rt_str(rt_dispatch_path(d.as_ptr(), d.len() as u32));
    let out = alloc::format!("{{\"hello\":\"{path}\"}}");
    rt_response(200, out.as_ptr(), out.len() as u32);

    rt_status_done(1)
}

/// The composite-identity half the layering gives the tenant: which runtime world this
/// app layer was compiled against.
#[no_mangle]
pub extern "C" fn app_runtime_world_len() -> u32 {
    wisp_rtabi::RUNTIME_WORLD.len() as u32
}
