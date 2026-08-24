//! The runtime layer's allocator (§4.2: "a GC operating entirely within linear
//! memory"; here a much simpler arena, but the structural point is the same —
//! the *runtime* owns the heap and the app layer has none of its own).
//!
//! The heap starts at `memory.size` as observed on the first allocation. For
//! the base layer that is deliberately 8 MiB, because the base is linked with
//! `--initial-memory=8388608` to reserve the window [4 MiB, 8 MiB) that an app
//! layer's static data and shadow stack are linked into (`--global-base`).
//! Nothing else in this file knows about that window; it falls out of the
//! link.

use core::alloc::{GlobalAlloc, Layout};

const CLASSES: usize = 10; // 16,32,64,...,8192 bytes
const MIN_CLASS_SHIFT: usize = 4;
const PAGE: usize = 64 * 1024;

struct Arena {
    /// Bump pointer. 0 = uninitialized.
    next: usize,
    /// One past the last usable byte of the memory we have grown to.
    end: usize,
    start: usize,
    free: [usize; CLASSES],
    live_bytes: usize,
    total_allocs: u64,
}

static mut ARENA: Arena =
    Arena { next: 0, end: 0, start: 0, free: [0; CLASSES], live_bytes: 0, total_allocs: 0 };

#[inline]
fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

#[inline]
fn class_of(size: usize) -> Option<usize> {
    if size > 1 << (MIN_CLASS_SHIFT + CLASSES - 1) {
        return None;
    }
    let mut c = 0;
    while (1usize << (MIN_CLASS_SHIFT + c)) < size {
        c += 1;
    }
    Some(c)
}

#[cfg(target_arch = "wasm32")]
fn mem_pages() -> usize {
    core::arch::wasm32::memory_size(0)
}
#[cfg(target_arch = "wasm32")]
fn mem_grow(pages: usize) -> bool {
    core::arch::wasm32::memory_grow(0, pages) != usize::MAX
}
#[cfg(not(target_arch = "wasm32"))]
fn mem_pages() -> usize {
    0
}
#[cfg(not(target_arch = "wasm32"))]
fn mem_grow(_p: usize) -> bool {
    false
}

unsafe fn ensure_init(a: &mut Arena) {
    if a.start == 0 {
        let base = mem_pages() * PAGE;
        a.start = base;
        a.next = base;
        a.end = base;
    }
}

unsafe fn grow_to(a: &mut Arena, want: usize) -> bool {
    if want <= a.end {
        return true;
    }
    // `memory.size` is the truth; other code (a host, another layer) may have
    // grown it since we last looked.
    let cur = mem_pages() * PAGE;
    if cur > a.end {
        a.end = cur;
        if want <= a.end {
            return true;
        }
    }
    let need = align_up(want - a.end, PAGE) / PAGE;
    if !mem_grow(need.max(1)) {
        return false;
    }
    a.end = mem_pages() * PAGE;
    want <= a.end
}

/// # Safety
/// Single-threaded guest (§5.4); there is no other mutator.
pub unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    #[allow(static_mut_refs)]
    let a = &mut *core::ptr::addr_of_mut!(ARENA);
    ensure_init(a);
    a.total_allocs += 1;
    let align = align.max(16);
    if let Some(c) = class_of(size.max(1)) {
        let head = a.free[c];
        if head != 0 && align <= 16 {
            a.free[c] = *(head as *const usize);
            a.live_bytes += 1 << (MIN_CLASS_SHIFT + c);
            return head as *mut u8;
        }
    }
    let want_size = match class_of(size.max(1)) {
        Some(c) => 1usize << (MIN_CLASS_SHIFT + c),
        None => align_up(size, 16),
    };
    let p = align_up(a.next, align);
    if !grow_to(a, p + want_size) {
        return core::ptr::null_mut();
    }
    a.next = p + want_size;
    a.live_bytes += want_size;
    p as *mut u8
}

/// # Safety
/// `ptr` must come from `alloc` with the same layout.
pub unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    if ptr.is_null() {
        return;
    }
    #[allow(static_mut_refs)]
    let a = &mut *core::ptr::addr_of_mut!(ARENA);
    if align.max(16) > 16 {
        return; // over-aligned blocks are not recycled; benchmark-grade.
    }
    if let Some(c) = class_of(size.max(1)) {
        *(ptr as *mut usize) = a.free[c];
        a.free[c] = ptr as usize;
        a.live_bytes = a.live_bytes.saturating_sub(1 << (MIN_CLASS_SHIFT + c));
    }
}

/// # Safety
/// See `alloc`.
pub unsafe fn realloc(ptr: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
    if let (Some(a), Some(b)) = (class_of(old.max(1)), class_of(new.max(1))) {
        if a == b {
            return ptr;
        }
    }
    let p = alloc(new, align);
    if !p.is_null() && !ptr.is_null() {
        core::ptr::copy_nonoverlapping(ptr, p, old.min(new));
        dealloc(ptr, old, align);
    }
    p
}

pub fn heap_start() -> usize {
    unsafe {
        #[allow(static_mut_refs)]
        let a = &mut *core::ptr::addr_of_mut!(ARENA);
        ensure_init(a);
        a.start
    }
}
pub fn heap_used() -> usize {
    unsafe { core::ptr::addr_of!(ARENA).read().next.saturating_sub(heap_start()) }
}
pub fn heap_allocs() -> u64 {
    unsafe { core::ptr::addr_of!(ARENA).read().total_allocs }
}
pub fn memory_bytes() -> usize {
    mem_pages() * PAGE
}

/// The global allocator every layer in this experiment uses, so that the only
/// thing the size comparison measures is trimming, never allocator choice.
pub struct RtAlloc;

unsafe impl GlobalAlloc for RtAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        alloc(l.size(), l.align())
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        dealloc(p, l.size(), l.align())
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        realloc(p, l.size(), l.align(), new)
    }
}
