//! Tenant 6: a log search endpoint. Reaches regex counting, whitespace normalization and line sorting.
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

const INDEX: &str = "edge-access";
const NUM: &str = r"[0-9]+(?:\.[0-9]+)?";
const SAMPLE: &str = "GET /a 200 1.5ms\nGET /b 404 0.8ms\nGET /a 200 1.5ms\nPOST /c 500 12.25ms\nGET /d 200 3.0ms\n";

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

    let body = take_out_after(rt_dispatch_body(d.as_ptr(), d.len() as u32));
    let doc = if body.is_empty() { SAMPLE.as_bytes() } else { body.as_slice() };
    let flat = take_out_after(rt_normalize_ws(doc.as_ptr(), doc.len() as u32));
    let h = rt_regex_compile(NUM.as_ptr(), NUM.len() as u32);
    let hits = rt_regex_count(h, flat.as_ptr(), flat.len() as u32);
    let first = take_out_after(rt_regex_find(h, flat.as_ptr(), flat.len() as u32));
    let sorted = take_out_after(rt_sort_lines(doc.as_ptr(), doc.len() as u32));
    let uniq = take_out_after(rt_dedup_lines(sorted.as_ptr(), sorted.len() as u32));
    let out = alloc::format!(
        "{{\"index\":\"{INDEX}\",\"hits\":{hits},\"first\":\"{}\",\"lines\":{},\"unique\":{}}}",
        core::str::from_utf8(&first).unwrap_or(""),
        sorted.split(|b| *b == b'\n').count(),
        uniq.split(|b| *b == b'\n').count()
    );
    rt_response(200, out.as_ptr(), out.len() as u32);

    rt_status_done(1)
}

/// The composite-identity half the layering gives the tenant: which runtime world this
/// app layer was compiled against.
#[no_mangle]
pub extern "C" fn app_runtime_world_len() -> u32 {
    wisp_rtabi::RUNTIME_WORLD.len() as u32
}
