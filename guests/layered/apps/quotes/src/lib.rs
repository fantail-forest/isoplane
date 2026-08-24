//! Tenant 1: an FX quote endpoint. Reaches JSON, float formatting and date formatting.
//!
//! Written once, built twice: `--features layered` compiles it against the
//! §4.6 runtime layer's imported ABI (the module then contains app code and
//! nothing else), `--features selfcontained` links the identical runtime code
//! in and lets fat LTO trim it to this app's reachable subset (§4.2). The two
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

const SYMBOL: &str = "NZD/USD";
const SAMPLE: &str = r#"{"quote":{"bid":0.6102,"ask":0.6118,"venue":"demo"},"seq":41}"#;

/// Copy the runtime layer's out channel, but only if the call succeeded.
unsafe fn take_out_after(n: i32) -> Vec<u8> {
    if n < 0 { Vec::new() } else { take_out() }
}

#[allow(dead_code)]
unsafe fn rt_str(n: i32) -> alloc::string::String {
    alloc::string::String::from_utf8(take_out_after(n)).unwrap_or_default()
}

/// §5.2 host→guest buffer handoff. In `layered` mode this hands back a pointer
/// into the *runtime layer's* heap: the app owns no memory of its own.
#[no_mangle]
pub unsafe extern "C" fn wisp_alloc(size: u32, align: u32) -> *mut u8 {
    rt_alloc(size, align)
}

#[no_mangle]
pub unsafe extern "C" fn wisp_handle(ptr: *const u8, len: u32) -> u64 {
    let d = core::slice::from_raw_parts(ptr, len as usize);

    let path = rt_str(rt_dispatch_path(d.as_ptr(), d.len() as u32));
    let body = take_out_after(rt_dispatch_body(d.as_ptr(), d.len() as u32));
    let doc = if body.is_empty() { SAMPLE.as_bytes() } else { body.as_slice() };
    let mut bid = 0f64;
    let mut ask = 0f64;
    let kb = b"quote.bid";
    let ka = b"quote.ask";
    rt_json_get_f64(doc.as_ptr(), doc.len() as u32, kb.as_ptr(), kb.len() as u32, &mut bid);
    rt_json_get_f64(doc.as_ptr(), doc.len() as u32, ka.as_ptr(), ka.len() as u32, &mut ask);
    let mid = (bid + ask) / 2.0;
    let mids = rt_str(rt_fmt_f64(mid, 5));
    let now = rt_dispatch_now(d.as_ptr(), d.len() as u32);
    let stamp = rt_str(rt_fmt_date_iso(now));
    let out = alloc::format!(
        "{{\"path\":\"{path}\",\"symbol\":\"{SYMBOL}\",\"mid\":{mids},\"as_of\":\"{stamp}\"}}"
    );
    let n = rt_json_reserialize(out.as_ptr(), out.len() as u32);
    let canon = take_out_after(n);
    rt_response(200, canon.as_ptr(), canon.len() as u32);

    rt_status_done(1)
}

/// The composite-identity half §4.6 gives the tenant: which runtime world this
/// app layer was compiled against.
#[no_mangle]
pub extern "C" fn app_runtime_world_len() -> u32 {
    wisp_rtabi::RUNTIME_WORLD.len() as u32
}
