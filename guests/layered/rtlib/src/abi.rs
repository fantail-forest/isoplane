//! The runtime layer's exported ABI — §4.6's "tenant modules compile against
//! the runtime component's exported ABI and link at instantiation".
//!
//! Everything is C ABI over `(ptr, len)` in the shared linear memory, with one
//! out-channel (`rt_out_ptr`/`rt_out_len`) instead of multi-value returns, so
//! the surface is expressible in core wasm with no component-model tooling.
//! Handles (regex, map) are dense u32 indices into runtime-owned tables —
//! deliberately resource-shaped, because that is what a real
//! `wisp:dotnet-runtime@x.y` world would use.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ptr::addr_of_mut;

use base64::Engine as _;
use regex::Regex;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// export switch
// ---------------------------------------------------------------------------

/// `#[no_mangle]` only when this crate is the base layer. Off, the identical
/// code is an ordinary rlib and LTO is free to delete every function the app
/// does not reach.
macro_rules! rt {
    ($(#[$m:meta])* pub fn $n:ident($($a:ident: $t:ty),* $(,)?) $(-> $r:ty)? $b:block) => {
        $(#[$m])*
        #[cfg_attr(feature = "export-abi", no_mangle)]
        pub unsafe extern "C" fn $n($($a: $t),*) $(-> $r)? $b
    };
}

// ---------------------------------------------------------------------------
// runtime-owned state
// ---------------------------------------------------------------------------

static mut OUT: Vec<u8> = Vec::new();
static mut REGEXES: Vec<Regex> = Vec::new();
static mut MAPS: Vec<Vec<(Vec<u8>, Vec<u8>)>> = Vec::new();
static mut FUTURE_SEQ: u64 = 0;
static mut INITIALIZED: u32 = 0;

#[inline]
unsafe fn out() -> &'static mut Vec<u8> {
    &mut *addr_of_mut!(OUT)
}
#[inline]
unsafe fn put(b: &[u8]) -> i32 {
    let o = out();
    o.clear();
    o.extend_from_slice(b);
    b.len() as i32
}
#[inline]
unsafe fn sl<'a>(p: *const u8, l: u32) -> &'a [u8] {
    if p.is_null() || l == 0 {
        &[]
    } else {
        core::slice::from_raw_parts(p, l as usize)
    }
}
#[inline]
unsafe fn st<'a>(p: *const u8, l: u32) -> Option<&'a str> {
    core::str::from_utf8(sl(p, l)).ok()
}

// ---------------------------------------------------------------------------
// 1. memory + the out channel
// ---------------------------------------------------------------------------

rt! { pub fn rt_alloc(size: u32, align: u32) -> *mut u8 { crate::heap::alloc(size as usize, align as usize) } }
rt! { pub fn rt_dealloc(p: *mut u8, size: u32, align: u32) { crate::heap::dealloc(p, size as usize, align as usize) } }
rt! { pub fn rt_realloc(p: *mut u8, old: u32, align: u32, new: u32) -> *mut u8 {
    crate::heap::realloc(p, old as usize, align as usize, new as usize) } }
rt! { pub fn rt_out_ptr() -> *const u8 { out().as_ptr() } }
rt! { pub fn rt_out_len() -> u32 { out().len() as u32 } }
rt! { pub fn rt_heap_start() -> u32 { crate::heap::heap_start() as u32 } }
rt! { pub fn rt_heap_used() -> u32 { crate::heap::heap_used() as u32 } }
rt! { pub fn rt_memory_bytes() -> u32 { crate::heap::memory_bytes() as u32 } }
rt! { pub fn rt_allocs() -> u64 { crate::heap::heap_allocs() } }

// §4.4 pre-initialization entry for the base layer: everything the runtime
// can compute once per machine, computed once per machine. Wizer calls this
// and freezes the result as the base template.
rt! { pub fn wisp_initialize() {
    if INITIALIZED != 0 { return; }
    INITIALIZED = 1;
    out().reserve(64 * 1024);
    // Warm the runtime's own tables the way a managed runtime's static
    // constructors would: pre-compile the patterns the platform ships, build
    // the interned tables, touch the codec paths so their statics exist in the
    // template rather than being built per instance.
    for p in PRECOMPILED { let _ = rt_regex_compile(p.as_ptr(), p.len() as u32); }
    let _ = rt_map_new();
    let seed = b"wisp:dotnet-runtime@1.0 base template";
    let _ = rt_sha256(seed.as_ptr(), seed.len() as u32);
    let _ = rt_base64_encode(seed.as_ptr(), seed.len() as u32);
    let _ = rt_deflate(seed.as_ptr(), seed.len() as u32, 6);
    let j = br#"{"warm":true,"n":[1,2,3]}"#;
    let _ = rt_json_reserialize(j.as_ptr(), j.len() as u32);
    out().clear();
} }

const PRECOMPILED: &[&str] = &[
    r"^[A-Z]{3}/[A-Z]{3}$",
    r"\b[\w.+-]+@[\w-]+\.[\w.]+\b",
    r"^/(?:[A-Za-z0-9._~-]+/?)*$",
    r"\s+",
    r"[0-9]+(?:\.[0-9]+)?",
];

// ---------------------------------------------------------------------------
// 2. JSON  (serde_json)
// ---------------------------------------------------------------------------

rt! { pub fn rt_json_validate(p: *const u8, l: u32) -> i32 {
    match serde_json::from_slice::<serde_json::Value>(sl(p, l)) { Ok(_) => 0, Err(_) => -1 } } }

rt! { pub fn rt_json_reserialize(p: *const u8, l: u32) -> i32 {
    match serde_json::from_slice::<serde_json::Value>(sl(p, l)) {
        Ok(v) => match serde_json::to_vec(&v) { Ok(b) => put(&b), Err(_) => -2 },
        Err(_) => -1,
    } } }

rt! { pub fn rt_json_pretty(p: *const u8, l: u32) -> i32 {
    match serde_json::from_slice::<serde_json::Value>(sl(p, l)) {
        Ok(v) => match serde_json::to_vec_pretty(&v) { Ok(b) => put(&b), Err(_) => -2 },
        Err(_) => -1,
    } } }

unsafe fn json_at<'v>(v: &'v serde_json::Value, path: &str) -> Option<&'v serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            serde_json::Value::Object(m) => m.get(seg)?,
            serde_json::Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

rt! { pub fn rt_json_get(p: *const u8, l: u32, pp: *const u8, pl: u32) -> i32 {
    let (Ok(v), Some(path)) = (serde_json::from_slice::<serde_json::Value>(sl(p, l)), st(pp, pl)) else { return -1 };
    match json_at(&v, path) {
        Some(serde_json::Value::String(s)) => put(s.as_bytes()),
        Some(other) => match serde_json::to_vec(other) { Ok(b) => put(&b), Err(_) => -2 },
        None => -3,
    } } }

rt! { pub fn rt_json_get_f64(p: *const u8, l: u32, pp: *const u8, pl: u32, out_v: *mut f64) -> i32 {
    let (Ok(v), Some(path)) = (serde_json::from_slice::<serde_json::Value>(sl(p, l)), st(pp, pl)) else { return -1 };
    match json_at(&v, path).and_then(|x| x.as_f64()) { Some(f) => { *out_v = f; 0 } None => -3 } } }

rt! { pub fn rt_json_nodes(p: *const u8, l: u32) -> i32 {
    fn count(v: &serde_json::Value) -> i32 {
        1 + match v {
            serde_json::Value::Array(a) => a.iter().map(count).sum(),
            serde_json::Value::Object(m) => m.values().map(count).sum(),
            _ => 0,
        }
    }
    match serde_json::from_slice::<serde_json::Value>(sl(p, l)) { Ok(v) => count(&v), Err(_) => -1 } } }

rt! { pub fn rt_json_merge(a: *const u8, al: u32, b: *const u8, bl: u32) -> i32 {
    let (Ok(mut x), Ok(y)) = (
        serde_json::from_slice::<serde_json::Value>(sl(a, al)),
        serde_json::from_slice::<serde_json::Value>(sl(b, bl)),
    ) else { return -1 };
    fn merge(x: &mut serde_json::Value, y: serde_json::Value) {
        match (x, y) {
            (serde_json::Value::Object(m), serde_json::Value::Object(n)) => {
                for (k, v) in n { merge(m.entry(k).or_insert(serde_json::Value::Null), v); }
            }
            (slot, v) => *slot = v,
        }
    }
    merge(&mut x, y);
    match serde_json::to_vec(&x) { Ok(v) => put(&v), Err(_) => -2 } } }

rt! { pub fn rt_json_escape(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    match serde_json::to_vec(&serde_json::Value::String(s.to_string())) { Ok(v) => put(&v), Err(_) => -2 } } }

// ---------------------------------------------------------------------------
// 3. regex
// ---------------------------------------------------------------------------

rt! { pub fn rt_regex_compile(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    match Regex::new(s) {
        Ok(r) => {
            let t: &mut Vec<Regex> = &mut *addr_of_mut!(REGEXES);
            t.push(r);
            (t.len() - 1) as i32
        }
        Err(_) => -2,
    } } }

unsafe fn rx(h: i32) -> Option<&'static Regex> {
    let t: &'static [Regex] = &*addr_of_mut!(REGEXES);
    t.get(h as usize)
}

rt! { pub fn rt_regex_is_match(h: i32, p: *const u8, l: u32) -> i32 {
    let (Some(r), Some(s)) = (rx(h), st(p, l)) else { return -1 };
    r.is_match(s) as i32 } }

rt! { pub fn rt_regex_count(h: i32, p: *const u8, l: u32) -> i32 {
    let (Some(r), Some(s)) = (rx(h), st(p, l)) else { return -1 };
    r.find_iter(s).count() as i32 } }

rt! { pub fn rt_regex_find(h: i32, p: *const u8, l: u32) -> i32 {
    let (Some(r), Some(s)) = (rx(h), st(p, l)) else { return -1 };
    match r.find(s) { Some(m) => put(m.as_str().as_bytes()), None => -3 } } }

rt! { pub fn rt_regex_capture(h: i32, p: *const u8, l: u32, group: u32) -> i32 {
    let (Some(r), Some(s)) = (rx(h), st(p, l)) else { return -1 };
    match r.captures(s).and_then(|c| c.get(group as usize)) {
        Some(m) => put(m.as_str().as_bytes()), None => -3 } } }

rt! { pub fn rt_regex_replace_all(h: i32, p: *const u8, l: u32, rp: *const u8, rl: u32) -> i32 {
    let (Some(r), Some(s), Some(rep)) = (rx(h), st(p, l), st(rp, rl)) else { return -1 };
    put(r.replace_all(s, rep).as_bytes()) } }

rt! { pub fn rt_regex_split_count(h: i32, p: *const u8, l: u32) -> i32 {
    let (Some(r), Some(s)) = (rx(h), st(p, l)) else { return -1 };
    r.split(s).count() as i32 } }

// ---------------------------------------------------------------------------
// 4. hashing
// ---------------------------------------------------------------------------

rt! { pub fn rt_sha256(p: *const u8, l: u32) -> i32 {
    let mut h = Sha256::new(); h.update(sl(p, l)); put(&h.finalize()) } }

rt! { pub fn rt_hmac_sha256(k: *const u8, kl: u32, m: *const u8, ml: u32) -> i32 {
    let mut key = [0u8; 64];
    let kb = sl(k, kl);
    if kb.len() > 64 { let mut h = Sha256::new(); h.update(kb); key[..32].copy_from_slice(&h.finalize()); }
    else { key[..kb.len()].copy_from_slice(kb); }
    let mut ipad = [0x36u8; 64]; let mut opad = [0x5cu8; 64];
    for i in 0..64 { ipad[i] ^= key[i]; opad[i] ^= key[i]; }
    let mut h = Sha256::new(); h.update(ipad); h.update(sl(m, ml));
    let inner = h.finalize();
    let mut h2 = Sha256::new(); h2.update(opad); h2.update(inner);
    put(&h2.finalize()) } }

rt! { pub fn rt_crc32(p: *const u8, l: u32) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in sl(p, l) {
        c ^= b as u32;
        for _ in 0..8 { c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 }; }
    }
    !c } }

rt! { pub fn rt_fnv1a64(p: *const u8, l: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in sl(p, l) { h ^= b as u64; h = h.wrapping_mul(0x1000_0000_01b3); }
    h } }

// ---------------------------------------------------------------------------
// 5. codecs
// ---------------------------------------------------------------------------

rt! { pub fn rt_base64_encode(p: *const u8, l: u32) -> i32 {
    put(base64::engine::general_purpose::STANDARD.encode(sl(p, l)).as_bytes()) } }

rt! { pub fn rt_base64_decode(p: *const u8, l: u32) -> i32 {
    match base64::engine::general_purpose::STANDARD.decode(sl(p, l)) { Ok(v) => put(&v), Err(_) => -1 } } }

rt! { pub fn rt_hex_encode(p: *const u8, l: u32) -> i32 {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut v = Vec::with_capacity(l as usize * 2);
    for &b in sl(p, l) { v.push(H[(b >> 4) as usize]); v.push(H[(b & 15) as usize]); }
    put(&v) } }

rt! { pub fn rt_hex_decode(p: *const u8, l: u32) -> i32 {
    let b = sl(p, l);
    if b.len() % 2 != 0 { return -1; }
    let mut v = Vec::with_capacity(b.len() / 2);
    for c in b.chunks(2) {
        let hi = (c[0] as char).to_digit(16); let lo = (c[1] as char).to_digit(16);
        match (hi, lo) { (Some(a), Some(c2)) => v.push((a * 16 + c2) as u8), _ => return -1 }
    }
    put(&v) } }

rt! { pub fn rt_url_encode(p: *const u8, l: u32) -> i32 {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    let mut v = Vec::with_capacity(l as usize);
    for &b in sl(p, l) {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { v.push(b); }
        else { v.push(b'%'); v.push(H[(b >> 4) as usize]); v.push(H[(b & 15) as usize]); }
    }
    put(&v) } }

rt! { pub fn rt_url_decode(p: *const u8, l: u32) -> i32 {
    let b = sl(p, l);
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16); let lo = (b[i + 2] as char).to_digit(16);
            match (hi, lo) { (Some(a), Some(c)) => { v.push((a * 16 + c) as u8); i += 3; continue } _ => return -1 }
        }
        v.push(b[i]); i += 1;
    }
    put(&v) } }

// ---------------------------------------------------------------------------
// 6. compression
// ---------------------------------------------------------------------------

rt! { pub fn rt_deflate(p: *const u8, l: u32, level: u32) -> i32 {
    put(&miniz_oxide::deflate::compress_to_vec(sl(p, l), level.min(10) as u8)) } }

rt! { pub fn rt_inflate(p: *const u8, l: u32) -> i32 {
    match miniz_oxide::inflate::decompress_to_vec_with_limit(sl(p, l), 8 << 20) {
        Ok(v) => put(&v), Err(_) => -1 } } }

// ---------------------------------------------------------------------------
// 7. text
// ---------------------------------------------------------------------------

rt! { pub fn rt_upper(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 }; put(s.to_uppercase().as_bytes()) } }
rt! { pub fn rt_lower(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 }; put(s.to_lowercase().as_bytes()) } }
rt! { pub fn rt_trim(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 }; put(s.trim().as_bytes()) } }
rt! { pub fn rt_utf8_valid(p: *const u8, l: u32) -> i32 { st(p, l).is_some() as i32 } }

rt! { pub fn rt_normalize_ws(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    let mut o = String::with_capacity(s.len());
    let mut sp = false;
    for c in s.chars() {
        if c.is_whitespace() { if !sp && !o.is_empty() { o.push(' '); sp = true; } }
        else { o.push(c); sp = false; }
    }
    put(o.trim_end().as_bytes()) } }

rt! { pub fn rt_str_find(h: *const u8, hl: u32, n: *const u8, nl: u32) -> i32 {
    let (Some(a), Some(b)) = (st(h, hl), st(n, nl)) else { return -1 };
    match a.find(b) { Some(i) => i as i32, None => -3 } } }

rt! { pub fn rt_sort_lines(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    let mut v: Vec<&str> = s.lines().collect();
    v.sort_unstable();
    put(v.join("\n").as_bytes()) } }

rt! { pub fn rt_dedup_lines(p: *const u8, l: u32) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    let mut v: Vec<&str> = s.lines().collect();
    v.sort_unstable(); v.dedup();
    put(v.join("\n").as_bytes()) } }

// ---------------------------------------------------------------------------
// 8. formatting
// ---------------------------------------------------------------------------

rt! { pub fn rt_fmt_f64(v: f64, prec: u32) -> i32 {
    put(alloc::format!("{:.*}", prec as usize, v).as_bytes()) } }
rt! { pub fn rt_fmt_i64(v: i64) -> i32 { put(v.to_string().as_bytes()) } }
rt! { pub fn rt_parse_f64(p: *const u8, l: u32, o: *mut f64) -> i32 {
    let Some(s) = st(p, l) else { return -1 };
    match s.trim().parse::<f64>() { Ok(f) => { *o = f; 0 } Err(_) => -1 } } }

// Civil-from-days (Howard Hinnant's algorithm), so the runtime layer owns
// date formatting the way a BCL would.
rt! { pub fn rt_fmt_date_iso(ms: i64) -> i32 {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    put(alloc::format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, tod / 3600, (tod / 60) % 60, tod % 60, ms.rem_euclid(1000)
    ).as_bytes()) } }

rt! { pub fn rt_parse_date_iso(p: *const u8, l: u32) -> i64 {
    let Some(s) = st(p, l) else { return i64::MIN };
    let b = s.as_bytes();
    if b.len() < 19 { return i64::MIN; }
    let n = |a: usize, z: usize| -> i64 { s[a..z].parse::<i64>().unwrap_or(-1) };
    let (y, m, d) = (n(0, 4), n(5, 7), n(8, 10));
    if y < 0 || m < 1 || d < 1 { return i64::MIN; }
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + n(11, 13) * 3600 + n(14, 16) * 60 + n(17, 19)) * 1000 } }

// ---------------------------------------------------------------------------
// 9. collections
// ---------------------------------------------------------------------------

rt! { pub fn rt_map_new() -> i32 {
    let t: &mut Vec<Vec<(Vec<u8>, Vec<u8>)>> = &mut *addr_of_mut!(MAPS);
    t.push(Vec::new());
    (t.len() - 1) as i32 } }

unsafe fn map(h: i32) -> Option<&'static mut Vec<(Vec<u8>, Vec<u8>)>> {
    let t: &'static mut Vec<Vec<(Vec<u8>, Vec<u8>)>> = &mut *addr_of_mut!(MAPS);
    t.get_mut(h as usize)
}

rt! { pub fn rt_map_set(h: i32, k: *const u8, kl: u32, v: *const u8, vl: u32) -> i32 {
    let Some(m) = map(h) else { return -1 };
    let (kb, vb) = (sl(k, kl).to_vec(), sl(v, vl).to_vec());
    match m.binary_search_by(|e| e.0.as_slice().cmp(&kb)) {
        Ok(i) => { m[i].1 = vb; 1 }
        Err(i) => { m.insert(i, (kb, vb)); 0 }
    } } }

rt! { pub fn rt_map_get(h: i32, k: *const u8, kl: u32) -> i32 {
    let Some(m) = map(h) else { return -1 };
    let kb = sl(k, kl);
    match m.binary_search_by(|e| e.0.as_slice().cmp(kb)) { Ok(i) => { let v = m[i].1.clone(); put(&v) } Err(_) => -3 } } }

rt! { pub fn rt_map_del(h: i32, k: *const u8, kl: u32) -> i32 {
    let Some(m) = map(h) else { return -1 };
    let kb = sl(k, kl);
    match m.binary_search_by(|e| e.0.as_slice().cmp(kb)) { Ok(i) => { m.remove(i); 1 } Err(_) => 0 } } }

rt! { pub fn rt_map_len(h: i32) -> i32 { match map(h) { Some(m) => m.len() as i32, None => -1 } } }

rt! { pub fn rt_map_keys(h: i32) -> i32 {
    let Some(m) = map(h) else { return -1 };
    let mut o = Vec::new();
    for (i, (k, _)) in m.iter().enumerate() { if i > 0 { o.push(b'\n'); } o.extend_from_slice(k); }
    put(&o) } }

// ---------------------------------------------------------------------------
// 10. the wisp:platform dispatch codec + future table
//     (§4.6: "GC, scheduler and wake machinery" — this is the wake machinery's
//     serialization half, which is the part a Rust stand-in can carry.)
// ---------------------------------------------------------------------------

rt! { pub fn rt_dispatch_kind(p: *const u8, l: u32) -> i32 {
    match wisp_abi::decode::<wisp_abi::dispatch::DispatchRecord>(sl(p, l)) {
        Ok(d) => match d.kind {
            wisp_abi::dispatch::DispatchKind::Http(_) => 0,
            wisp_abi::dispatch::DispatchKind::DiMessage { .. } => 1,
            wisp_abi::dispatch::DispatchKind::Alarm { .. } => 2,
            wisp_abi::dispatch::DispatchKind::QueueBatch { .. } => 3,
            wisp_abi::dispatch::DispatchKind::CronTick { .. } => 4,
            _ => 5,
        },
        Err(_) => -1,
    } } }

rt! { pub fn rt_dispatch_path(p: *const u8, l: u32) -> i32 {
    match wisp_abi::decode::<wisp_abi::dispatch::DispatchRecord>(sl(p, l)) {
        Ok(d) => match d.kind {
            wisp_abi::dispatch::DispatchKind::Http(h) => put(h.path.as_bytes()),
            _ => -3,
        },
        Err(_) => -1,
    } } }

rt! { pub fn rt_dispatch_body(p: *const u8, l: u32) -> i32 {
    match wisp_abi::decode::<wisp_abi::dispatch::DispatchRecord>(sl(p, l)) {
        Ok(d) => match d.kind {
            wisp_abi::dispatch::DispatchKind::Http(h) => put(&h.body),
            _ => -3,
        },
        Err(_) => -1,
    } } }

rt! { pub fn rt_dispatch_budget(p: *const u8, l: u32) -> i32 {
    match wisp_abi::decode::<wisp_abi::dispatch::DispatchRecord>(sl(p, l)) {
        Ok(d) => d.cpu_budget_ms as i32, Err(_) => -1 } } }

rt! { pub fn rt_dispatch_now(p: *const u8, l: u32) -> i64 {
    match wisp_abi::decode::<wisp_abi::dispatch::DispatchRecord>(sl(p, l)) {
        Ok(d) => d.now_ms as i64, Err(_) => -1 } } }

rt! { pub fn rt_response(status: u32, body: *const u8, bl: u32) -> i32 {
    let head = wisp_abi::dispatch::ResponseHead {
        status: status as u16,
        headers: alloc::vec![("content-type".to_string(), "application/json".to_string())],
        body: sl(body, bl).to_vec(),
        streaming: false,
    };
    match wisp_abi::encode(&head) { Ok(v) => put(&v), Err(_) => -2 } } }

// The future table §4.2 puts in the runtime layer: the app never sees an id
// it did not get from here.
rt! { pub fn rt_future_next() -> u64 { FUTURE_SEQ += 1; FUTURE_SEQ } }

rt! { pub fn rt_status_done(response: u32) -> u64 {
    wisp_abi::status::PollStatus::Done { response }.encode() } }
rt! { pub fn rt_status_pending() -> u64 { wisp_abi::status::PollStatus::Pending.encode() } }
