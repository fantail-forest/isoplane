//! The guest heap: a first-fit, address-ordered, coalescing arena that knows
//! how to make itself cheap to snapshot (spec.md §5.5, §6.6).
//!
//! ## What this is, honestly
//!
//! §4.2 specifies a bundled generational GC with a mark-compact snapshot mode.
//! A Rust guest has no precise root set, so **moving** live objects is not
//! possible and pretending otherwise would be a lie. What *is* possible, and
//! is what §6.6 actually pays for, is the other half of the snapshot mode:
//!
//! > *the guest GC performs mark-compact into low addresses, zeroes the freed
//! > tail, and resets card tables. Zeroed pages compress out of the delta.*
//!
//! So on `wisp_gc_hint(snapshot)` this allocator:
//!
//! 1. **rolls the bump pointer back** over any free run that reaches the top
//!    of the heap -- compaction toward low addresses in the only sense a
//!    non-moving allocator can offer;
//! 2. **zeroes every free block's body** and the tail between the bump pointer
//!    and the heap's high-water mark, so those pages are runs of zeros in the
//!    hibernation delta and zstd removes them;
//! 3. **remembers which blocks it already zeroed** (a bit stolen from the
//!    size word), so a second `gc_hint` with no intervening allocation dirties
//!    nothing. Writing zeros over an already-zero page still trips the
//!    soft-dirty PTE the host diffs against, so "zero everything, always"
//!    would make the delta *bigger*.
//!
//! Nothing here shrinks linear memory: shipped WASM has no page-return
//! instruction, and §6.6 does not ask for one -- *compaction through rebirth*
//! (hibernate, then wake onto a fresh CoW template) is the platform's
//! defragmenter, and this allocator's job is only to make the delta small.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;

const WORD: usize = core::mem::size_of::<usize>();
/// Every block starts 16-aligned, which covers every alignment Rust's own
/// collections ask for.
const ALIGN: usize = 16;
/// Room for the free-block header (`size`, `next`).
const MIN_BLOCK: usize = 16;
/// Stolen from the size word: sizes are 16-aligned, so the low 4 bits are
/// free. Set means "this block's body is already all zeros" (see §6.6 above).
const ZEROED: usize = 1;

#[inline]
const fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// A free block, stored in-place at the head of the free run.
#[repr(C)]
struct FreeBlock {
    /// Byte length of the whole run, including this header. Low bit is [`ZEROED`].
    size: usize,
    /// Next run, strictly higher in address, or 0.
    next: usize,
}

/// Reported by [`heap_stats`] and by `wisp_gc_hint`; the demo guests surface
/// these so delta-size tests have something to assert on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes handed out and not yet freed.
    pub live_bytes: usize,
    /// Bytes sitting in the free list.
    pub free_bytes: usize,
    /// Current top of the arena.
    pub brk: usize,
    /// Highest the top has ever been. Pages above this were never written by
    /// the guest and are still template-clean.
    pub high_water: usize,
    /// Bytes of linear memory owned by the arena.
    pub arena_bytes: usize,
    /// Bytes the last `gc_hint` reclaimed by rolling `brk` back.
    pub last_tail_reclaimed: usize,
    /// Bytes the last `gc_hint` actually wrote zeros over.
    pub last_zeroed: usize,
}

pub struct Heap {
    base: usize,
    brk: usize,
    end: usize,
    high_water: usize,
    free: usize,
    live_bytes: usize,
    last_tail_reclaimed: usize,
    last_zeroed: usize,
    grow: fn(&mut Heap, usize) -> bool,
}

impl Heap {
    pub const fn new() -> Heap {
        Heap {
            base: 0,
            brk: 0,
            end: 0,
            high_water: 0,
            free: 0,
            live_bytes: 0,
            last_tail_reclaimed: 0,
            last_zeroed: 0,
            grow: sys_grow,
        }
    }

    /// A heap over a caller-supplied region. Used by the SDK's host-side unit
    /// tests, which must not replace the test runner's own allocator.
    pub fn over(region: &mut [u8]) -> Heap {
        let base = align_up(region.as_ptr() as usize, ALIGN);
        let end = region.as_ptr() as usize + region.len();
        Heap {
            base,
            brk: base,
            end,
            high_water: base,
            free: 0,
            live_bytes: 0,
            last_tail_reclaimed: 0,
            last_zeroed: 0,
            grow: |_, _| false,
        }
    }

    pub fn stats(&self) -> HeapStats {
        let mut free_bytes = 0;
        let mut p = self.free;
        while p != 0 {
            let b = unsafe { &*(p as *const FreeBlock) };
            free_bytes += b.size & !ZEROED;
            p = b.next;
        }
        HeapStats {
            live_bytes: self.live_bytes,
            free_bytes,
            brk: self.brk,
            high_water: self.high_water,
            arena_bytes: self.end.saturating_sub(self.base),
            last_tail_reclaimed: self.last_tail_reclaimed,
            last_zeroed: self.last_zeroed,
        }
    }

    #[inline]
    fn block_size(layout: &Layout) -> usize {
        align_up(layout.size().max(MIN_BLOCK), ALIGN)
    }

    /// # Safety
    /// Standard `GlobalAlloc::alloc` contract.
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        if layout.align() > ALIGN {
            return self.alloc_overaligned(layout);
        }
        let need = Self::block_size(&layout);
        if let Some(p) = self.take_from_free(need) {
            self.live_bytes += need;
            return p as *mut u8;
        }
        if self.brk + need > self.end && !(self.grow)(self, self.brk + need - self.end) {
            return ptr::null_mut();
        }
        let p = self.brk;
        self.brk += need;
        if self.brk > self.high_water {
            self.high_water = self.brk;
        }
        self.live_bytes += need;
        p as *mut u8
    }

    /// Alignments above 16 are rare (they come from `#[repr(align(N))]`
    /// types). Over-allocate and stash the true block address one word below
    /// the pointer we hand out.
    unsafe fn alloc_overaligned(&mut self, layout: Layout) -> *mut u8 {
        let a = layout.align();
        let raw_layout = Layout::from_size_align(layout.size() + a + WORD, ALIGN).unwrap();
        let raw = self.alloc(raw_layout);
        if raw.is_null() {
            return raw;
        }
        let aligned = align_up(raw as usize + WORD, a);
        ptr::write((aligned - WORD) as *mut usize, raw as usize);
        aligned as *mut u8
    }

    /// # Safety
    /// Standard `GlobalAlloc::dealloc` contract.
    pub unsafe fn dealloc(&mut self, p: *mut u8, layout: Layout) {
        if layout.align() > ALIGN {
            let raw = ptr::read((p as usize - WORD) as *const usize);
            let raw_layout =
                Layout::from_size_align(layout.size() + layout.align() + WORD, ALIGN).unwrap();
            return self.dealloc(raw as *mut u8, raw_layout);
        }
        let need = Self::block_size(&layout);
        self.live_bytes -= need;
        self.insert_free(p as usize, need);
    }

    /// # Safety
    /// Standard `GlobalAlloc::realloc` contract.
    pub unsafe fn realloc(&mut self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.align() > ALIGN {
            return self.realloc_copy(p, layout, new_size);
        }
        let old = Self::block_size(&layout);
        let new = align_up(new_size.max(MIN_BLOCK), ALIGN);
        if new == old {
            return p;
        }
        let top = p as usize + old;
        // Growing the most recent allocation in place is the common `Vec`
        // push pattern; doing it here keeps the arena from fragmenting into
        // a staircase of abandoned buffers, which is what would otherwise
        // dominate a hibernation delta.
        if top == self.brk {
            if new > old {
                let extra = new - old;
                if self.brk + extra > self.end && !(self.grow)(self, self.brk + extra - self.end) {
                    return ptr::null_mut();
                }
            }
            self.brk = p as usize + new;
            if self.brk > self.high_water {
                self.high_water = self.brk;
            }
            self.live_bytes = self.live_bytes + new - old;
            return p;
        }
        if new < old {
            self.insert_free(top - (old - new), old - new);
            self.live_bytes -= old - new;
            return p;
        }
        self.realloc_copy(p, layout, new_size)
    }

    unsafe fn realloc_copy(&mut self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let nl = Layout::from_size_align_unchecked(new_size, layout.align());
        let q = self.alloc(nl);
        if !q.is_null() {
            ptr::copy_nonoverlapping(p, q, layout.size().min(new_size));
            self.dealloc(p, layout);
        }
        q
    }

    /// First fit over the address-ordered list. Splitting keeps the remainder
    /// at the high end so ordering is preserved without a re-walk.
    unsafe fn take_from_free(&mut self, need: usize) -> Option<usize> {
        let mut prevp: *mut usize = &mut self.free;
        let mut p = self.free;
        while p != 0 {
            let b = &mut *(p as *mut FreeBlock);
            let size = b.size & !ZEROED;
            let next = b.next;
            if size >= need {
                if size - need >= MIN_BLOCK {
                    let rest = p + need;
                    // The remainder keeps the parent's zeroed status: we are
                    // handing out the low part, and the tail is untouched.
                    let zeroed = b.size & ZEROED;
                    ptr::write(rest as *mut FreeBlock, FreeBlock { size: (size - need) | zeroed, next });
                    *prevp = rest;
                } else {
                    *prevp = next;
                }
                return Some(p);
            }
            prevp = &mut (*(p as *mut FreeBlock)).next;
            p = next;
        }
        None
    }

    /// Insert `[at, at+len)` into the address-ordered list, merging with the
    /// runs on either side. Coalescing on free is what lets `gc_hint` reclaim
    /// a contiguous tail instead of a sieve.
    unsafe fn insert_free(&mut self, at: usize, len: usize) {
        let mut prev: usize = 0;
        let mut p = self.free;
        while p != 0 && p < at {
            prev = p;
            p = (*(p as *const FreeBlock)).next;
        }
        let mut size = len;
        let mut next = p;
        // Merge forward.
        if p != 0 && at + size == p {
            let nb = &*(p as *const FreeBlock);
            size += nb.size & !ZEROED;
            next = nb.next;
        }
        // Merge backward.
        if prev != 0 {
            let pb = &mut *(prev as *mut FreeBlock);
            let psize = pb.size & !ZEROED;
            if prev + psize == at {
                pb.size = psize + size; // freshly freed body: no longer known-zero
                pb.next = next;
                return;
            }
        }
        ptr::write(at as *mut FreeBlock, FreeBlock { size, next });
        if prev == 0 {
            self.free = at;
        } else {
            (*(prev as *mut FreeBlock)).next = at;
        }
    }

    /// §5.5 / §6.6. See the module docs for what this does and does not do.
    ///
    /// `zero` is false for the low-pressure hint, where writing zeros would
    /// cost more dirty pages than it saves.
    pub fn compact_and_zero(&mut self, zero: bool) -> HeapStats {
        self.last_tail_reclaimed = 0;
        self.last_zeroed = 0;

        // 1. Roll `brk` back over a free run that reaches the top. Because the
        //    list is coalesced there is at most one such run, and it is last.
        unsafe {
            let mut prevp: *mut usize = &mut self.free;
            let mut p = self.free;
            while p != 0 {
                let b = &*(p as *const FreeBlock);
                let size = b.size & !ZEROED;
                let next = b.next;
                if next == 0 && p + size == self.brk {
                    *prevp = 0;
                    self.brk = p;
                    self.last_tail_reclaimed = size;
                    break;
                }
                prevp = &mut (*(p as *mut FreeBlock)).next;
                p = next;
            }
        }

        if !zero {
            return self.stats();
        }

        // 2. Zero the body of every free run that is not already zero, and
        //    mark it. The 16-byte header stays live -- it is the list.
        unsafe {
            let mut p = self.free;
            while p != 0 {
                let b = &mut *(p as *mut FreeBlock);
                let size = b.size & !ZEROED;
                let next = b.next;
                if b.size & ZEROED == 0 && size > MIN_BLOCK {
                    ptr::write_bytes((p + MIN_BLOCK) as *mut u8, 0, size - MIN_BLOCK);
                    self.last_zeroed += size - MIN_BLOCK;
                    b.size = size | ZEROED;
                }
                p = next;
            }
        }

        // 3. Zero from the live top up to the high-water mark. Pages above
        //    the high-water mark were never written by this instance, so they
        //    are still template pages and must not be touched.
        if self.high_water > self.brk {
            unsafe { ptr::write_bytes(self.brk as *mut u8, 0, self.high_water - self.brk) };
            self.last_zeroed += self.high_water - self.brk;
            self.high_water = self.brk;
        }
        self.stats()
    }
}

#[cfg(target_arch = "wasm32")]
fn sys_grow(h: &mut Heap, need: usize) -> bool {
    const WASM_PAGE: usize = 65_536;
    let pages = align_up(need, WASM_PAGE) / WASM_PAGE;
    let prev = core::arch::wasm32::memory_grow::<0>(pages);
    if prev == usize::MAX {
        return false; // §11.1 instance memory ceiling; surfaces as OOM
    }
    let new_base = prev * WASM_PAGE;
    if h.end == 0 {
        h.base = new_base;
        h.brk = new_base;
        h.high_water = new_base;
    }
    // We are the only allocator in the module, so growth is always contiguous
    // with what we already own. If it ever is not, restarting the arena at the
    // new region leaks the old one but stays correct.
    if h.end != 0 && new_base != h.end {
        h.brk = new_base;
        h.high_water = new_base;
        h.free = 0;
    }
    h.end = new_base + pages * WASM_PAGE;
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn sys_grow(_h: &mut Heap, _need: usize) -> bool {
    false
}

// ---------------------------------------------------------------------------
// The global allocator, installed on wasm only.
// ---------------------------------------------------------------------------

struct HeapCell(UnsafeCell<Heap>);
// Single logical thread by construction: §4.3 rule V2 turns threads, shared
// memory and atomics off at the engine, so the module cannot even express a
// second thread.
unsafe impl Sync for HeapCell {}

static HEAP: HeapCell = HeapCell(UnsafeCell::new(Heap::new()));

#[inline]
#[allow(clippy::mut_from_ref)]
fn heap() -> &'static mut Heap {
    unsafe { &mut *HEAP.0.get() }
}

impl Default for Heap {
    fn default() -> Heap {
        Heap::new()
    }
}

pub struct WispAlloc;

unsafe impl GlobalAlloc for WispAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        heap().alloc(layout)
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        heap().dealloc(p, layout)
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        heap().realloc(p, layout, new_size)
    }
}

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static GLOBAL: WispAlloc = WispAlloc;

/// Snapshot of the guest heap. Demo guests expose this over HTTP so the
/// host-side delta-size tests have a guest-visible number to compare against.
pub fn heap_stats() -> HeapStats {
    heap().stats()
}

/// Called by the generated `wisp_gc_hint` export.
pub(crate) fn gc_hint(pressure: crate::GcPressure) -> HeapStats {
    heap().compact_and_zero(pressure.should_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> &'static mut [u8] {
        Box::leak(vec![0u8; 1 << 20].into_boxed_slice())
    }

    unsafe fn l(n: usize) -> Layout {
        Layout::from_size_align_unchecked(n, 8)
    }

    #[test]
    fn alloc_free_reuses_and_coalesces() {
        let mut h = Heap::over(region());
        unsafe {
            let a = h.alloc(l(64));
            let b = h.alloc(l(64));
            let c = h.alloc(l(64));
            assert_eq!(b as usize, a as usize + 64);
            assert_eq!(c as usize, b as usize + 64);
            h.dealloc(a, l(64));
            h.dealloc(b, l(64));
            // a and b coalesced into one 128-byte run, so a 128 fits at `a`.
            let d = h.alloc(l(128));
            assert_eq!(d as usize, a as usize);
            assert_eq!(h.stats().free_bytes, 0);
            h.dealloc(c, l(64));
            h.dealloc(d, l(128));
        }
        assert_eq!(h.stats().live_bytes, 0);
    }

    /// The "compaction toward low addresses" the spec can actually buy from a
    /// non-moving allocator: a free run touching the top gives the bump
    /// pointer back.
    #[test]
    fn gc_hint_rolls_back_a_free_tail() {
        let mut h = Heap::over(region());
        unsafe {
            let a = h.alloc(l(4096));
            let b = h.alloc(l(4096));
            h.dealloc(b, l(4096));
            let before = h.stats().brk;
            let s = h.compact_and_zero(true);
            assert_eq!(s.last_tail_reclaimed, 4096);
            assert_eq!(s.brk, before - 4096);
            assert_eq!(s.free_bytes, 0);
            h.dealloc(a, l(4096));
        }
    }

    #[test]
    fn gc_hint_zeroes_free_bodies_and_the_tail() {
        let mut h = Heap::over(region());
        unsafe {
            let a = h.alloc(l(4096));
            ptr::write_bytes(a, 0xAB, 4096);
            let keep = h.alloc(l(64));
            h.dealloc(a, l(4096));
            let s = h.compact_and_zero(true);
            // header stays, body is zeroed
            assert_eq!(s.last_zeroed, 4096 - MIN_BLOCK);
            let body = core::slice::from_raw_parts(a.add(MIN_BLOCK), 4096 - MIN_BLOCK);
            assert!(body.iter().all(|&x| x == 0));
            h.dealloc(keep, l(64));
        }
    }

    /// Re-zeroing a page that is already zero still trips the host's
    /// soft-dirty PTE walk, so the second hint must write nothing.
    #[test]
    fn a_second_gc_hint_dirties_nothing() {
        let mut h = Heap::over(region());
        unsafe {
            let a = h.alloc(l(8192));
            let keep = h.alloc(l(64));
            h.dealloc(a, l(8192));
            let first = h.compact_and_zero(true);
            assert!(first.last_zeroed > 0);
            let second = h.compact_and_zero(true);
            assert_eq!(second.last_zeroed, 0, "gc_hint must be idempotent on a clean heap");
            h.dealloc(keep, l(64));
        }
    }

    #[test]
    fn realloc_grows_the_top_block_in_place() {
        let mut h = Heap::over(region());
        unsafe {
            let a = h.alloc(l(64));
            let b = h.realloc(a, l(64), 256);
            assert_eq!(a as usize, b as usize, "top-block growth must not copy");
            h.dealloc(b, l(256));
        }
    }

    #[test]
    fn overaligned_allocations_round_trip() {
        let mut h = Heap::over(region());
        unsafe {
            let lay = Layout::from_size_align_unchecked(100, 128);
            let p = h.alloc(lay);
            assert_eq!(p as usize % 128, 0);
            ptr::write_bytes(p, 0x5A, 100);
            h.dealloc(p, lay);
            assert_eq!(h.stats().live_bytes, 0);
        }
    }

    #[test]
    fn exhaustion_returns_null_not_a_panic() {
        let mut h = Heap::over(Box::leak(vec![0u8; 8192].into_boxed_slice()));
        unsafe {
            assert!(!h.alloc(l(4096)).is_null());
            // §11.1: the plan ceiling shows up here as a failed grow. The
            // guest must surface OutOfMemory, not corrupt itself.
            assert!(h.alloc(l(1 << 20)).is_null());
        }
    }
}
