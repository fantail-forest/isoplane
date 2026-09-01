//! One tenant-facing surface, two link strategies: layered, and self-contained.
//!
//! Every app in `guests/layered/apps` is written once against this crate and
//! built twice. The *only* thing that differs is whether `surface!` below
//! expands to wasm imports resolved against the shared base layer at
//! instantiation, or to direct calls into a statically linked `wisp-rtlib`
//! that fat LTO is then free to trim to the reachable subset.
//!
//! Both builds use the same allocator (`wisp-rtlib`'s arena, reached through
//! `rt_alloc`), so the size difference this measures is trimming and nothing
//! else. That is deliberately conservative: a real self-contained module also
//! ships its own allocator, which this comparison hands it for free.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::vec::Vec;

#[cfg(all(feature = "layered", feature = "selfcontained"))]
compile_error!("pick exactly one of `layered` / `selfcontained`");
#[cfg(not(any(feature = "layered", feature = "selfcontained")))]
compile_error!("pick exactly one of `layered` / `selfcontained`");

/// The single source of truth for the runtime layer's ABI. Both the tenant
/// view (imports, below) and the base layer's `#[no_mangle]` export wrappers
/// (`guests/layered/base`) are generated from this one list, so the two sides
/// of the boundary cannot drift.
#[macro_export]
macro_rules! rt_surface_list {
    ($m:ident) => { $m! {
    fn rt_alloc(size: u32, align: u32) -> *mut u8;
    fn rt_dealloc(p: *mut u8, size: u32, align: u32) -> ();
    fn rt_realloc(p: *mut u8, old: u32, align: u32, new: u32) -> *mut u8;
    fn rt_out_ptr() -> *const u8;
    fn rt_out_len() -> u32;
    fn rt_heap_start() -> u32;
    fn rt_heap_used() -> u32;
    fn rt_memory_bytes() -> u32;
    fn rt_allocs() -> u64;

    fn rt_json_validate(p: *const u8, l: u32) -> i32;
    fn rt_json_reserialize(p: *const u8, l: u32) -> i32;
    fn rt_json_pretty(p: *const u8, l: u32) -> i32;
    fn rt_json_get(p: *const u8, l: u32, pp: *const u8, pl: u32) -> i32;
    fn rt_json_get_f64(p: *const u8, l: u32, pp: *const u8, pl: u32, out_v: *mut f64) -> i32;
    fn rt_json_nodes(p: *const u8, l: u32) -> i32;
    fn rt_json_merge(a: *const u8, al: u32, b: *const u8, bl: u32) -> i32;
    fn rt_json_escape(p: *const u8, l: u32) -> i32;

    fn rt_regex_compile(p: *const u8, l: u32) -> i32;
    fn rt_regex_is_match(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_count(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_find(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_capture(h: i32, p: *const u8, l: u32, group: u32) -> i32;
    fn rt_regex_replace_all(h: i32, p: *const u8, l: u32, rp: *const u8, rl: u32) -> i32;
    fn rt_regex_split_count(h: i32, p: *const u8, l: u32) -> i32;

    fn rt_sha256(p: *const u8, l: u32) -> i32;
    fn rt_hmac_sha256(k: *const u8, kl: u32, m: *const u8, ml: u32) -> i32;
    fn rt_crc32(p: *const u8, l: u32) -> u32;
    fn rt_fnv1a64(p: *const u8, l: u32) -> u64;

    fn rt_base64_encode(p: *const u8, l: u32) -> i32;
    fn rt_base64_decode(p: *const u8, l: u32) -> i32;
    fn rt_hex_encode(p: *const u8, l: u32) -> i32;
    fn rt_hex_decode(p: *const u8, l: u32) -> i32;
    fn rt_url_encode(p: *const u8, l: u32) -> i32;
    fn rt_url_decode(p: *const u8, l: u32) -> i32;

    fn rt_deflate(p: *const u8, l: u32, level: u32) -> i32;
    fn rt_inflate(p: *const u8, l: u32) -> i32;

    fn rt_upper(p: *const u8, l: u32) -> i32;
    fn rt_lower(p: *const u8, l: u32) -> i32;
    fn rt_trim(p: *const u8, l: u32) -> i32;
    fn rt_utf8_valid(p: *const u8, l: u32) -> i32;
    fn rt_normalize_ws(p: *const u8, l: u32) -> i32;
    fn rt_str_find(h: *const u8, hl: u32, n: *const u8, nl: u32) -> i32;
    fn rt_sort_lines(p: *const u8, l: u32) -> i32;
    fn rt_dedup_lines(p: *const u8, l: u32) -> i32;

    fn rt_fmt_f64(v: f64, prec: u32) -> i32;
    fn rt_fmt_i64(v: i64) -> i32;
    fn rt_parse_f64(p: *const u8, l: u32, o: *mut f64) -> i32;
    fn rt_fmt_date_iso(ms: i64) -> i32;
    fn rt_parse_date_iso(p: *const u8, l: u32) -> i64;

    fn rt_map_new() -> i32;
    fn rt_map_set(h: i32, k: *const u8, kl: u32, v: *const u8, vl: u32) -> i32;
    fn rt_map_get(h: i32, k: *const u8, kl: u32) -> i32;
    fn rt_map_del(h: i32, k: *const u8, kl: u32) -> i32;
    fn rt_map_len(h: i32) -> i32;
    fn rt_map_keys(h: i32) -> i32;

    fn rt_dispatch_kind(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_path(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_body(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_budget(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_now(p: *const u8, l: u32) -> i64;
    fn rt_response(status: u32, body: *const u8, bl: u32) -> i32;
    fn rt_future_next() -> u64;
    fn rt_status_done(response: u32) -> u64;
    fn rt_status_pending() -> u64;
}  };
}

macro_rules! surface {
    ($( fn $n:ident ( $($a:ident : $t:ty),* $(,)? ) -> $r:ty; )*) => {
        // Layered: the runtime is an *import*. Note the module name is
        // `env`, which is also where `wasm-ld --import-memory` puts the
        // imported linear memory, so one `Linker::instance("env", base)` in
        // the host satisfies the whole runtime world at once.
        #[cfg(feature = "layered")]
        #[link(wasm_import_module = "env")]
        extern "C" {
            $( pub fn $n($($a: $t),*) -> $r; )*
        }

        // Self-contained: the identical functions, statically linked.
        #[cfg(feature = "selfcontained")]
        pub use wisp_rtlib::abi::{ $($n),* };
    };
}

surface! {
    fn rt_alloc(size: u32, align: u32) -> *mut u8;
    fn rt_dealloc(p: *mut u8, size: u32, align: u32) -> ();
    fn rt_realloc(p: *mut u8, old: u32, align: u32, new: u32) -> *mut u8;
    fn rt_out_ptr() -> *const u8;
    fn rt_out_len() -> u32;
    fn rt_heap_start() -> u32;
    fn rt_heap_used() -> u32;
    fn rt_memory_bytes() -> u32;
    fn rt_allocs() -> u64;

    fn rt_json_validate(p: *const u8, l: u32) -> i32;
    fn rt_json_reserialize(p: *const u8, l: u32) -> i32;
    fn rt_json_pretty(p: *const u8, l: u32) -> i32;
    fn rt_json_get(p: *const u8, l: u32, pp: *const u8, pl: u32) -> i32;
    fn rt_json_get_f64(p: *const u8, l: u32, pp: *const u8, pl: u32, out_v: *mut f64) -> i32;
    fn rt_json_nodes(p: *const u8, l: u32) -> i32;
    fn rt_json_merge(a: *const u8, al: u32, b: *const u8, bl: u32) -> i32;
    fn rt_json_escape(p: *const u8, l: u32) -> i32;

    fn rt_regex_compile(p: *const u8, l: u32) -> i32;
    fn rt_regex_is_match(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_count(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_find(h: i32, p: *const u8, l: u32) -> i32;
    fn rt_regex_capture(h: i32, p: *const u8, l: u32, group: u32) -> i32;
    fn rt_regex_replace_all(h: i32, p: *const u8, l: u32, rp: *const u8, rl: u32) -> i32;
    fn rt_regex_split_count(h: i32, p: *const u8, l: u32) -> i32;

    fn rt_sha256(p: *const u8, l: u32) -> i32;
    fn rt_hmac_sha256(k: *const u8, kl: u32, m: *const u8, ml: u32) -> i32;
    fn rt_crc32(p: *const u8, l: u32) -> u32;
    fn rt_fnv1a64(p: *const u8, l: u32) -> u64;

    fn rt_base64_encode(p: *const u8, l: u32) -> i32;
    fn rt_base64_decode(p: *const u8, l: u32) -> i32;
    fn rt_hex_encode(p: *const u8, l: u32) -> i32;
    fn rt_hex_decode(p: *const u8, l: u32) -> i32;
    fn rt_url_encode(p: *const u8, l: u32) -> i32;
    fn rt_url_decode(p: *const u8, l: u32) -> i32;

    fn rt_deflate(p: *const u8, l: u32, level: u32) -> i32;
    fn rt_inflate(p: *const u8, l: u32) -> i32;

    fn rt_upper(p: *const u8, l: u32) -> i32;
    fn rt_lower(p: *const u8, l: u32) -> i32;
    fn rt_trim(p: *const u8, l: u32) -> i32;
    fn rt_utf8_valid(p: *const u8, l: u32) -> i32;
    fn rt_normalize_ws(p: *const u8, l: u32) -> i32;
    fn rt_str_find(h: *const u8, hl: u32, n: *const u8, nl: u32) -> i32;
    fn rt_sort_lines(p: *const u8, l: u32) -> i32;
    fn rt_dedup_lines(p: *const u8, l: u32) -> i32;

    fn rt_fmt_f64(v: f64, prec: u32) -> i32;
    fn rt_fmt_i64(v: i64) -> i32;
    fn rt_parse_f64(p: *const u8, l: u32, o: *mut f64) -> i32;
    fn rt_fmt_date_iso(ms: i64) -> i32;
    fn rt_parse_date_iso(p: *const u8, l: u32) -> i64;

    fn rt_map_new() -> i32;
    fn rt_map_set(h: i32, k: *const u8, kl: u32, v: *const u8, vl: u32) -> i32;
    fn rt_map_get(h: i32, k: *const u8, kl: u32) -> i32;
    fn rt_map_del(h: i32, k: *const u8, kl: u32) -> i32;
    fn rt_map_len(h: i32) -> i32;
    fn rt_map_keys(h: i32) -> i32;

    fn rt_dispatch_kind(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_path(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_body(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_budget(p: *const u8, l: u32) -> i32;
    fn rt_dispatch_now(p: *const u8, l: u32) -> i64;
    fn rt_response(status: u32, body: *const u8, bl: u32) -> i32;
    fn rt_future_next() -> u64;
    fn rt_status_done(response: u32) -> u64;
    fn rt_status_pending() -> u64;
}

// ---------------------------------------------------------------------------
// The allocator. In `layered` mode the app has no allocator of its own at all:
// every byte it owns lives in the runtime layer's heap: the heap belongs to
// the runtime, not to the app.
// ---------------------------------------------------------------------------

pub struct RtAlloc;

unsafe impl core::alloc::GlobalAlloc for RtAlloc {
    unsafe fn alloc(&self, l: core::alloc::Layout) -> *mut u8 {
        rt_alloc(l.size() as u32, l.align() as u32)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: core::alloc::Layout) {
        rt_dealloc(p, l.size() as u32, l.align() as u32)
    }
    unsafe fn realloc(&self, p: *mut u8, l: core::alloc::Layout, new: usize) -> *mut u8 {
        rt_realloc(p, l.size() as u32, l.align() as u32, new as u32)
    }
}

/// Copy whatever the last runtime call left in the out channel.
pub fn take_out() -> Vec<u8> {
    unsafe {
        let (p, n) = (rt_out_ptr(), rt_out_len() as usize);
        if p.is_null() || n == 0 {
            return Vec::new();
        }
        core::slice::from_raw_parts(p, n).to_vec()
    }
}

/// The layer identity: which runtime ABI this app was compiled against. A
/// base-layer republish that changes this string changes the composite
/// `(base hash, app hash)` and therefore invalidates hibernated deltas.
pub const RUNTIME_WORLD: &str = "wisp:rtlib@1.0";
