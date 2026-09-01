//! Synthetic JSON audit messages using the runtime layer's HMAC, date and map APIs.
//! The layered and selfcontained features select imported or linked runtime implementations.

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

const STREAM: &str = "billing-audit";
const KEY: &str = "escrowed-per-deploy-key-not-a-real-secret";
const SAMPLE: &str = r#"{"actor":"svc-invoicer","action":"issue","amount":1299,"currency":"NZD"}"#;

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
    let k = b"actor";
    let actor = take_out_after(rt_json_get(doc.as_ptr(), doc.len() as u32, k.as_ptr(), k.len() as u32));
    let mac = take_out_after(rt_hmac_sha256(KEY.as_ptr(), KEY.len() as u32, doc.as_ptr(), doc.len() as u32));
    let hex = take_out_after(rt_hex_encode(mac.as_ptr(), mac.len() as u32));
    let now = rt_dispatch_now(d.as_ptr(), d.len() as u32);
    let stamp = rt_str(rt_fmt_date_iso(now));
    let m = rt_map_new();
    rt_map_set(m, k.as_ptr(), k.len() as u32, actor.as_ptr(), actor.len() as u32);
    let out = alloc::format!(
        "{{\"stream\":\"{STREAM}\",\"actor\":\"{}\",\"at\":\"{stamp}\",\"sig\":\"{}\",\"entries\":{}}}",
        core::str::from_utf8(&actor).unwrap_or(""),
        core::str::from_utf8(&hex).unwrap_or(""),
        rt_map_len(m)
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
