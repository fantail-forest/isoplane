//! Tenant 2: a chat room that redacts and normalizes messages. Reaches regex, case mapping and the runtime's map.
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

const ROOM: &str = "lobby";
const EMAIL: &str = r"\b[\w.+-]+@[\w-]+\.[\w.]+\b";
const MASK: &str = "<redacted>";
const SAMPLE: &str = "ping me at ada@example.test or bob.j+tag@mail.example.co.nz, thanks";

/// Copy the runtime layer's out channel, but only if the call succeeded.
unsafe fn take_out_after(n: i32) -> Vec<u8> {
    if n < 0 { Vec::new() } else { take_out() }
}

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

    let body = take_out_after(rt_dispatch_body(d.as_ptr(), d.len() as u32));
    let msg = if body.is_empty() { SAMPLE.as_bytes() } else { body.as_slice() };
    let h = rt_regex_compile(EMAIL.as_ptr(), EMAIL.len() as u32);
    let hit = rt_regex_is_match(h, msg.as_ptr(), msg.len() as u32);
    let n = rt_regex_replace_all(h, msg.as_ptr(), msg.len() as u32, MASK.as_ptr(), MASK.len() as u32);
    let redacted = take_out_after(n);
    let shout = take_out_after(rt_upper(redacted.as_ptr(), redacted.len() as u32));
    let m = rt_map_new();
    let k = b"last";
    rt_map_set(m, k.as_ptr(), k.len() as u32, shout.as_ptr(), shout.len() as u32);
    let stored = take_out_after(rt_map_get(m, k.as_ptr(), k.len() as u32));
    let out = alloc::format!(
        "{{\"room\":\"{ROOM}\",\"redacted\":{hit},\"len\":{},\"text\":\"{}\"}}",
        stored.len(),
        core::str::from_utf8(&stored).unwrap_or("")
    );
    rt_response(200, out.as_ptr(), out.len() as u32);

    rt_status_done(1)
}

/// The composite-identity half §4.6 gives the tenant: which runtime world this
/// app layer was compiled against.
#[no_mangle]
pub extern "C" fn app_runtime_world_len() -> u32 {
    wisp_rtabi::RUNTIME_WORLD.len() as u32
}
