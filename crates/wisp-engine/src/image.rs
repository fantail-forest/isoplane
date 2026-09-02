//! The template's post-init linear memory, stored as its non-zero byte runs.
//!
//! A pre-initialised .NET template declares tens of megabytes of which a
//! fraction of a percent is content, and the GC writes a few dozen bytes at
//! the head and tail of every 64 KiB region, so no 4 KiB page is entirely
//! zero: a dense `Vec<u8>` charges the whole span and a sparse-page map saves
//! almost nothing. Byte runs store what is there. The one operation the
//! hibernation page diff needs, [`MemoryImage::page_eq`], is a binary search
//! plus a walk of the runs overlapping one page.

/// The unit hibernation diffs in.
pub const PAGE: usize = wisp_abi::PAGE_SIZE;

/// Zero gaps shorter than this stay inside a run. A separate run costs an
/// index entry and a binary-search step, more than a few zero bytes.
const COALESCE_GAP: usize = 32;

/// A logical length plus the non-zero runs; everything else is zero, which is
/// what unwritten wasm memory reads as. `PartialEq` is logical equality:
/// [`MemoryImage::from_dense`] produces one canonical run set for a memory.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct MemoryImage {
    len: usize,
    /// Sorted by offset and non-overlapping. One payload `Vec` for all runs
    /// rather than one per run: a .NET template has thousands.
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
    pub fn from_dense(dense: &[u8]) -> Self {
        let mut runs: Vec<Run> = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut i = 0usize;
        while i < dense.len() {
            if dense[i] == 0 {
                i += 1;
                continue;
            }
            let start = i;
            let mut end = i + 1;
            let mut j = end;
            while j < dense.len() {
                if dense[j] != 0 {
                    end = j + 1;
                    j = end;
                    continue;
                }
                let gap_end = (j + COALESCE_GAP).min(dense.len());
                match dense[j..gap_end].iter().position(|&b| b != 0) {
                    Some(k) => j += k,
                    None => break,
                }
            }
            runs.push(Run {
                off: start,
                start: bytes.len(),
                len: end - start,
            });
            bytes.extend_from_slice(&dense[start..end]);
            i = end;
        }
        runs.shrink_to_fit();
        bytes.shrink_to_fit();
        MemoryImage {
            len: dense.len(),
            runs,
            bytes,
        }
    }

    /// The logical size of the memory, zeros included.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[cfg(test)]
    fn runs(&self) -> usize {
        self.runs.len()
    }

    /// Index of the first run whose end is strictly after `off`.
    fn first_run_from(&self, off: usize) -> usize {
        self.runs.partition_point(|r| r.off + r.len <= off)
    }

    /// Whether the live 4 KiB page at index `page` is byte-identical to the
    /// template's. A page beyond the image compares against zeros, which is
    /// what a guest that grew its memory sees there.
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

    /// Materialise the whole memory. The allocation this type exists to
    /// avoid; for tests and oracles, never a per-instance path.
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
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(dense: &[u8]) {
        let img = MemoryImage::from_dense(dense);
        assert_eq!(img.len(), dense.len(), "the logical length is preserved");
        assert_eq!(
            img.to_dense(),
            dense,
            "to_dense must reproduce the input exactly"
        );
        for p in 0..dense.len().div_ceil(PAGE) {
            let lo = p * PAGE;
            let hi = (lo + PAGE).min(dense.len());
            assert!(
                img.page_eq(p, &dense[lo..hi]),
                "page {p} of the image must compare equal to itself"
            );
        }
    }

    #[test]
    fn equality_is_logical() {
        let mut a = vec![0u8; 3 * PAGE];
        a[5] = 1;
        a[PAGE + 9] = 2;
        let b = a.clone();
        assert_eq!(
            MemoryImage::from_dense(&a),
            MemoryImage::from_dense(&b),
            "the same memory produces equal images"
        );
        let mut c = a.clone();
        c[PAGE + 9] = 3;
        assert_ne!(
            MemoryImage::from_dense(&a),
            MemoryImage::from_dense(&c),
            "a one-byte difference is a different image"
        );
        let mut d = a.clone();
        d.extend_from_slice(&[0u8; PAGE]);
        assert_ne!(
            MemoryImage::from_dense(&a),
            MemoryImage::from_dense(&d),
            "the same content at a different declared length is a different memory"
        );
    }

    #[test]
    fn empty_and_all_zero_images_hold_no_runs() {
        roundtrip(&[]);
        roundtrip(&[0u8; 9000]);
        assert_eq!(
            MemoryImage::from_dense(&[0u8; 9000]).runs(),
            0,
            "an all-zero memory stores nothing"
        );
    }

    #[test]
    fn content_with_no_zeros_is_one_run() {
        let d: Vec<u8> = (0..10_000u32).map(|i| (i % 255 + 1) as u8).collect();
        roundtrip(&d);
        assert_eq!(
            MemoryImage::from_dense(&d).runs(),
            1,
            "no zeros means one run"
        );
    }

    #[test]
    fn a_dotnet_shaped_image_stores_one_run_per_region_boundary() {
        let mut d = vec![0u8; 64 * 65536];
        for region in 0..64 {
            let base = region * 65536;
            d[base..base + 40].fill(0xAB);
            let tail = base + 65536 - 12;
            d[tail..base + 65536].fill(0xCD);
        }
        roundtrip(&d);
        // A region's tail run ends where the next region's head run begins,
        // so all but the first and last merge.
        assert_eq!(
            MemoryImage::from_dense(&d).runs(),
            65,
            "one run per region boundary, plus the two ends"
        );
    }

    #[test]
    fn short_zero_gaps_are_coalesced_and_long_ones_are_not() {
        let mut d = vec![0u8; 4096];
        d[10] = 1;
        d[10 + COALESCE_GAP - 1] = 1;
        d[2000] = 1;
        let img = MemoryImage::from_dense(&d);
        assert_eq!(
            img.runs(),
            2,
            "a gap inside the window joins its neighbours; a gap outside it splits"
        );
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
                assert!(
                    !img.page_eq(p, &live),
                    "page {p} byte {probe} must be seen as dirty"
                );
            }
        }
    }

    #[test]
    fn pages_beyond_the_image_compare_against_zero() {
        let img = MemoryImage::from_dense(&[1u8; PAGE]);
        assert!(
            img.page_eq(1, &[0u8; PAGE]),
            "grown memory that was never written is clean"
        );
        let mut live = [0u8; PAGE];
        live[7] = 1;
        assert!(
            !img.page_eq(1, &live),
            "grown memory that was written is dirty"
        );
    }

    #[test]
    fn a_partial_final_page_is_handled() {
        let mut d = vec![0u8; PAGE + 17];
        d[PAGE + 3] = 9;
        roundtrip(&d);
        let img = MemoryImage::from_dense(&d);
        assert!(
            img.page_eq(1, &d[PAGE..]),
            "the partial page compares equal to itself"
        );
        assert!(
            !img.page_eq(1, &[0u8; 17]),
            "zeroing the partial page's content is a difference"
        );
    }
}
