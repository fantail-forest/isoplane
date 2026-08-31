//! The template's post-init linear memory, stored as what is *in* it rather
//! than as the memory itself (spec.md §4.4, §6.6).
//!
//! ## Why this is not a `Vec<u8>`
//!
//! It was one, and for a Rust guest that is fine: a 17-page template is a
//! 1.1 MB allocation. For a pre-initialized .NET template it is not. The .NET
//! runtime grows linear memory to ~810 pages during startup, so wizer's
//! snapshot declares a 53,084,160-byte memory — of which the template's actual
//! content is **147,207 bytes in 2,536 data segments, 0.277 %**. The rest is
//! the GC's reserved-but-untouched heap.
//!
//! A dense `Vec<u8>` charges a cell 53 MB per template for 147 KB of content,
//! and it charges it *twice*, because `wisp-host`'s activator kept its own
//! copy for the hibernation baseline. Storing the runs instead makes both
//! copies the size of the content.
//!
//! ## Why runs and not pages
//!
//! Page granularity would be simpler and is the wrong shape here: the .NET GC
//! writes a few dozen bytes at the head and tail of every 64 KiB region, so
//! **no 4 KiB page of that image is entirely zero** — a sparse-page
//! representation would store 1,620 pages, 6.6 MB, and save almost nothing.
//! Byte runs store the 147 KB that is actually there.
//!
//! ## What it must still be able to do
//!
//! Exactly one thing, and cheaply: §6.6's page-diff asks "is this live 4 KiB
//! page identical to the template's?" for every candidate page. That is
//! [`MemoryImage::page_eq`], and it is a binary search plus a walk of the runs
//! overlapping one page.

/// Host page. The unit §6.6 diffs in, and deliberately not the 64 KiB wasm
/// page.
pub const PAGE: usize = 4096;

/// Zero runs shorter than this are kept *inside* a run rather than splitting
/// it. A separate run costs 24 bytes of index and a binary-search step, so
/// splitting to save 8 zero bytes loses on both counts; the .NET image's gaps
/// are mostly tens of kilobytes, which is far above this.
const COALESCE_GAP: usize = 32;

/// A template's linear memory: a logical length, plus the non-zero runs.
///
/// Everything not covered by a run is zero. That is not a compression trick,
/// it is what the memory *is* — wasm guarantees unwritten linear memory reads
/// as zero, so a byte outside every run has never been written by the guest's
/// initialization.
/// `PartialEq` is logical equality, not merely structural: [`from_dense`] is
/// deterministic and produces one canonical run set for any given memory, so
/// two images compare equal exactly when the memories they describe do.
///
/// [`from_dense`]: MemoryImage::from_dense
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MemoryImage {
    len: usize,
    /// `(offset, start_in_bytes, run_len)`, sorted by offset and
    /// non-overlapping. One `Vec` for the payload rather than one per run: a
    /// .NET template has ~2,500 runs, and 2,500 separate allocations is 2,500
    /// malloc headers and a fragmented heap for no gain.
    runs: Vec<Run>,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    off: usize,
    start: usize,
    len: usize,
}

impl MemoryImage {
    /// Scan a dense image once and keep only what is non-zero.
    ///
    /// The scan is O(len) with no allocation per zero byte, and it happens
    /// once per template at load time — against a 53 MB image that is a few
    /// milliseconds, paid once, to delete 53 MB held for the life of the cell.
    pub fn from_dense(dense: &[u8]) -> Self {
        let mut runs: Vec<Run> = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut i = 0usize;
        while i < dense.len() {
            if dense[i] == 0 {
                i += 1;
                continue;
            }
            // A run starts here. Extend it over non-zero bytes, and over any
            // zero gap shorter than COALESCE_GAP that is followed by more
            // content.
            let start = i;
            let mut end = i + 1;
            let mut j = end;
            while j < dense.len() {
                if dense[j] != 0 {
                    end = j + 1;
                    j = end;
                    continue;
                }
                // Look ahead across the gap.
                let gap_end = (j + COALESCE_GAP).min(dense.len());
                match dense[j..gap_end].iter().position(|&b| b != 0) {
                    Some(k) => j += k,
                    None => break,
                }
            }
            runs.push(Run { off: start, start: bytes.len(), len: end - start });
            bytes.extend_from_slice(&dense[start..end]);
            i = end;
        }
        runs.shrink_to_fit();
        bytes.shrink_to_fit();
        MemoryImage { len: dense.len(), runs, bytes }
    }

    /// The logical size of the memory, including everything that is zero.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes this actually holds — the number the density argument is about.
    pub fn stored_bytes(&self) -> usize {
        self.bytes.len() + self.runs.len() * std::mem::size_of::<Run>()
    }

    pub fn runs(&self) -> usize {
        self.runs.len()
    }

    /// Index of the first run whose end is strictly after `off`.
    fn first_run_from(&self, off: usize) -> usize {
        self.runs.partition_point(|r| r.off + r.len <= off)
    }

    /// One byte. Linear in nothing: a binary search and a bounds test.
    pub fn byte(&self, i: usize) -> u8 {
        if i >= self.len {
            return 0;
        }
        let k = self.first_run_from(i);
        match self.runs.get(k) {
            Some(r) if i >= r.off => self.bytes[r.start + (i - r.off)],
            _ => 0,
        }
    }

    /// Write the image's bytes for `[off, off + out.len())` into `out`.
    ///
    /// Bytes past [`len`] are zero, which is what wasm guarantees for memory a
    /// guest grew into but never wrote.
    pub fn read_at(&self, off: usize, out: &mut [u8]) {
        out.fill(0);
        let end = off + out.len();
        let mut k = self.first_run_from(off);
        while let Some(r) = self.runs.get(k) {
            if r.off >= end {
                break;
            }
            let lo = r.off.max(off);
            let hi = (r.off + r.len).min(end);
            if lo < hi {
                let src = r.start + (lo - r.off);
                out[lo - off..hi - off].copy_from_slice(&self.bytes[src..src + (hi - lo)]);
            }
            k += 1;
        }
    }

    /// §6.6's whole question: is this live page byte-identical to the
    /// template's?
    ///
    /// `page` is a 4 KiB index. A page beyond the image compares against zeros
    /// — wasm guarantees grown memory starts zero, so "equal to a zero-extended
    /// template" is the right test and is the same rule the dense version used.
    pub fn page_eq(&self, page: usize, live: &[u8]) -> bool {
        let off = page * PAGE;
        let mut cursor = off;
        let end = off + live.len();
        let mut k = self.first_run_from(off);
        while let Some(r) = self.runs.get(k) {
            if r.off >= end {
                break;
            }
            let lo = r.off.max(off);
            let hi = (r.off + r.len).min(end);
            if lo >= hi {
                k += 1;
                continue;
            }
            // Everything between the last run and this one must be zero in the
            // live page too.
            if live[cursor - off..lo - off].iter().any(|&b| b != 0) {
                return false;
            }
            let src = r.start + (lo - r.off);
            if live[lo - off..hi - off] != self.bytes[src..src + (hi - lo)] {
                return false;
            }
            cursor = hi;
            k += 1;
        }
        live[cursor - off..].iter().all(|&b| b == 0)
    }

    /// Materialize the whole thing. The escape hatch, and named so that a
    /// caller reaching for it in a hot or per-template path is visible in
    /// review: this is the allocation the type exists to avoid.
    pub fn to_dense(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len];
        for r in &self.runs {
            out[r.off..r.off + r.len].copy_from_slice(&self.bytes[r.start..r.start + r.len]);
        }
        out
    }
}

impl std::fmt::Debug for MemoryImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryImage")
            .field("len", &self.len)
            .field("runs", &self.runs.len())
            .field("stored_bytes", &self.stored_bytes())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(dense: &[u8]) {
        let img = MemoryImage::from_dense(dense);
        assert_eq!(img.len(), dense.len());
        assert_eq!(img.to_dense(), dense, "to_dense must reproduce the input exactly");
        for (i, &b) in dense.iter().enumerate() {
            assert_eq!(img.byte(i), b, "byte {i}");
        }
        for p in 0..dense.len().div_ceil(PAGE) {
            let lo = p * PAGE;
            let hi = (lo + PAGE).min(dense.len());
            let mut buf = vec![0u8; hi - lo];
            img.read_at(lo, &mut buf);
            assert_eq!(&buf[..], &dense[lo..hi], "read_at page {p}");
            assert!(img.page_eq(p, &dense[lo..hi]), "page_eq must hold for the image itself");
        }
    }

    #[test]
    fn equality_is_logical() {
        let mut a = vec![0u8; 3 * PAGE];
        a[5] = 1;
        a[PAGE + 9] = 2;
        let b = a.clone();
        assert_eq!(MemoryImage::from_dense(&a), MemoryImage::from_dense(&b));
        let mut c = a.clone();
        c[PAGE + 9] = 3;
        assert_ne!(MemoryImage::from_dense(&a), MemoryImage::from_dense(&c));
        // Same content, different declared length: not the same memory.
        let mut d = a.clone();
        d.extend_from_slice(&[0u8; PAGE]);
        assert_ne!(MemoryImage::from_dense(&a), MemoryImage::from_dense(&d));
    }

    #[test]
    fn empty_and_all_zero() {
        roundtrip(&[]);
        roundtrip(&[0u8; 9000]);
        assert_eq!(MemoryImage::from_dense(&[0u8; 9000]).runs(), 0);
    }

    #[test]
    fn dense_content() {
        let d: Vec<u8> = (0..10_000u32).map(|i| (i % 255 + 1) as u8).collect();
        roundtrip(&d);
        assert_eq!(MemoryImage::from_dense(&d).runs(), 1, "no zeros: one run");
    }

    /// The shape this type exists for: a few bytes at the head and tail of
    /// every 64 KiB region, which is what a pre-initialized .NET template is.
    #[test]
    fn dotnet_shaped_image_is_stored_sparsely() {
        let mut d = vec![0u8; 64 * 65536];
        for region in 0..64 {
            let base = region * 65536;
            d[base..base + 40].fill(0xAB);
            let tail = base + 65536 - 12;
            d[tail..base + 65536].fill(0xCD);
        }
        roundtrip(&d);
        let img = MemoryImage::from_dense(&d);
        // 65, not 128: a region's tail run ends exactly where the next
        // region's head run begins, so all but the first and last merge. That
        // adjacency is real in the .NET image too, and coalescing it is the
        // representation doing its job.
        assert_eq!(img.runs(), 65, "one run per region boundary, plus the two ends");
        assert!(
            img.stored_bytes() < d.len() / 500,
            "stored {} of {} bytes — the whole point",
            img.stored_bytes(),
            d.len()
        );
    }

    #[test]
    fn short_zero_gaps_are_coalesced_long_ones_are_not() {
        let mut d = vec![0u8; 4096];
        d[10] = 1;
        d[10 + COALESCE_GAP - 1] = 1; // inside the gap window
        d[2000] = 1; // far away
        let img = MemoryImage::from_dense(&d);
        assert_eq!(img.runs(), 2);
        roundtrip(&d);
    }

    #[test]
    fn page_eq_detects_every_single_byte_change() {
        let mut d = vec![0u8; 3 * PAGE];
        d[0] = 1;
        d[PAGE + 100] = 2;
        d[2 * PAGE + PAGE - 1] = 3;
        let img = MemoryImage::from_dense(&d);
        for p in 0..3 {
            for probe in [0usize, 1, 100, 4000, PAGE - 1] {
                let mut live = d[p * PAGE..(p + 1) * PAGE].to_vec();
                live[probe] = live[probe].wrapping_add(1);
                assert!(!img.page_eq(p, &live), "page {p} byte {probe} must be seen as dirty");
            }
        }
    }

    /// A page past the end of the image is compared against zeros, because
    /// that is what a guest that grew its memory sees there.
    #[test]
    fn pages_beyond_the_image_compare_against_zero() {
        let img = MemoryImage::from_dense(&[1u8; PAGE]);
        assert!(img.page_eq(1, &[0u8; PAGE]));
        let mut live = [0u8; PAGE];
        live[7] = 1;
        assert!(!img.page_eq(1, &live));
    }

    #[test]
    fn a_partial_final_page_is_handled() {
        let mut d = vec![0u8; PAGE + 17];
        d[PAGE + 3] = 9;
        roundtrip(&d);
        let img = MemoryImage::from_dense(&d);
        assert!(img.page_eq(1, &d[PAGE..]));
        assert!(!img.page_eq(1, &[0u8; 17]));
    }
}
