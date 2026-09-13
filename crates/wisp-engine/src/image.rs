//! Sparse template memory with a persisted index and demand-readable payload.

use std::sync::Arc;

/// The unit hibernation diffs in.
pub const PAGE: usize = wisp_abi::PAGE_SIZE;
const COALESCE_GAP: usize = 32;
const MAGIC: &[u8; 8] = b"WISPSPR1";
const HEADER_BYTES: usize = 32;
const RUN_BYTES: usize = 24;

pub(crate) enum ImageStorage {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl AsRef<[u8]> for ImageStorage {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped(bytes) => bytes,
        }
    }
}

/// Sparse memory shared by templates and instances; zero gaps allocate no payload.
#[derive(Clone)]
pub struct MemoryImage {
    len: usize,
    run_count: usize,
    payload_offset: usize,
    storage: Arc<ImageStorage>,
}

#[derive(Clone, Copy)]
struct Run {
    off: usize,
    start: usize,
    len: usize,
}

impl MemoryImage {
    /// Iterates stored regions in logical address order, without expanding zero gaps.
    pub fn regions(&self) -> impl Iterator<Item = (usize, &[u8])> {
        (0..self.run_count).map(|index| {
            let run = self.run(index);
            (
                run.off,
                &self.encoded()
                    [self.payload_offset + run.start..self.payload_offset + run.start + run.len],
            )
        })
    }

    /// Scans a dense image once during preparation.
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
        let mut encoded = Vec::with_capacity(HEADER_BYTES + runs.len() * RUN_BYTES + bytes.len());
        encoded.extend_from_slice(MAGIC);
        for value in [dense.len(), runs.len(), bytes.len()] {
            encoded.extend_from_slice(&(value as u64).to_le_bytes());
        }
        for run in &runs {
            for value in [run.off, run.start, run.len] {
                encoded.extend_from_slice(&(value as u64).to_le_bytes());
            }
        }
        encoded.extend_from_slice(&bytes);
        Self {
            len: dense.len(),
            run_count: runs.len(),
            payload_offset: HEADER_BYTES + runs.len() * RUN_BYTES,
            storage: std::sync::Arc::new(ImageStorage::Owned(encoded)),
        }
    }

    pub(crate) fn encoded(&self) -> &[u8] {
        self.storage.as_ref().as_ref()
    }

    pub(crate) fn from_storage(
        storage: ImageStorage,
        max_memory_bytes: usize,
    ) -> Result<Self, &'static str> {
        let bytes = storage.as_ref();
        if bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err("invalid sparse baseline magic");
        }
        let len = number(bytes, 8)?;
        let run_count = number(bytes, 16)?;
        let payload_bytes = number(bytes, 24)?;
        let payload_offset = run_count
            .checked_mul(RUN_BYTES)
            .and_then(|n| n.checked_add(HEADER_BYTES))
            .ok_or("sparse index overflow")?;
        if len > max_memory_bytes || payload_offset.checked_add(payload_bytes) != Some(bytes.len())
        {
            return Err("sparse baseline length exceeds its bounds");
        }
        let mut previous_end = 0;
        let mut payload_end = 0;
        for index in 0..run_count {
            let position = HEADER_BYTES + index * RUN_BYTES;
            let off = number(bytes, position)?;
            let start = number(bytes, position + 8)?;
            let size = number(bytes, position + 16)?;
            let end = off
                .checked_add(size)
                .ok_or("sparse memory range overflow")?;
            if size == 0 || off < previous_end || end > len || start != payload_end {
                return Err("sparse regions are overlapping, unordered or out of bounds");
            }
            payload_end = start
                .checked_add(size)
                .ok_or("sparse payload range overflow")?;
            if payload_end > payload_bytes {
                return Err("sparse region exceeds payload");
            }
            previous_end = end;
        }
        if payload_end != payload_bytes {
            return Err("sparse payload contains unindexed bytes");
        }
        Ok(Self {
            len,
            run_count,
            payload_offset,
            storage: Arc::new(storage),
        })
    }

    fn run(&self, index: usize) -> Run {
        let position = HEADER_BYTES + index * RUN_BYTES;
        // The constructor validates every index before the immutable storage is retained.
        let field = |offset| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&self.encoded()[offset..offset + 8]);
            u64::from_le_bytes(bytes) as usize
        };
        Run {
            off: field(position),
            start: field(position + 8),
            len: field(position + 16),
        }
    }

    /// Logical memory length, including zero gaps.
    pub fn len(&self) -> usize {
        self.len
    }
    /// Whether the logical memory is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[cfg(test)]
    fn runs(&self) -> usize {
        self.run_count
    }

    /// Compares a live page with the baseline, treating grown memory as zero.
    pub fn page_eq(&self, page: usize, live: &[u8]) -> bool {
        let Some(off) = page.checked_mul(PAGE) else {
            return false;
        };
        let Some(end) = off.checked_add(live.len()) else {
            return false;
        };
        let mut low = 0;
        let mut high = self.run_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let run = self.run(mid);
            if run.off + run.len <= off {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let mut cursor = off;
        for index in low..self.run_count {
            let run = self.run(index);
            if run.off >= end {
                break;
            }
            let lo = run.off.max(off);
            let hi = (run.off + run.len).min(end);
            if live[cursor - off..lo - off].iter().any(|&byte| byte != 0) {
                return false;
            }
            let src = self.payload_offset + run.start + lo - run.off;
            if live[lo - off..hi - off] != self.encoded()[src..src + hi - lo] {
                return false;
            }
            cursor = hi;
        }
        live[cursor - off..].iter().all(|&byte| byte == 0)
    }

    /// Expands the baseline for explicit inspection; activation never calls this.
    pub fn to_dense(&self) -> Vec<u8> {
        let mut out = vec![0; self.len];
        for (offset, bytes) in self.regions() {
            out[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        out
    }
}

fn number(bytes: &[u8], offset: usize) -> Result<usize, &'static str> {
    let field = bytes
        .get(offset..offset + 8)
        .ok_or("truncated sparse baseline")?;
    let mut value = [0; 8];
    value.copy_from_slice(field);
    usize::try_from(u64::from_le_bytes(value)).map_err(|_| "sparse field exceeds address space")
}

impl Default for MemoryImage {
    fn default() -> Self {
        Self::from_dense(&[])
    }
}
impl PartialEq for MemoryImage {
    fn eq(&self, other: &Self) -> bool {
        self.encoded() == other.encoded()
    }
}
impl Eq for MemoryImage {}
impl std::fmt::Debug for MemoryImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryImage")
            .field("len", &self.len)
            .field("runs", &self.run_count)
            .field("bytes", &self.encoded().len())
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
    fn persisted_sparse_format_has_stable_bytes() {
        let image = MemoryImage::from_dense(&[0, 9, 0]);
        let golden: &[u8] = b"WISPSPR1\x03\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x09";
        assert_eq!(
            image.encoded(),
            golden,
            "the versioned sparse encoding must not drift"
        );
        let decoded =
            MemoryImage::from_storage(ImageStorage::Owned(golden.to_vec()), 3).expect("decode");
        assert_eq!(
            decoded.to_dense(),
            [0, 9, 0],
            "persisted index must restore logical memory"
        );
    }

    #[test]
    fn malformed_persisted_indexes_are_rejected_before_page_comparison() {
        let mut dense = vec![0; 100];
        dense[1] = 7;
        dense[99] = 8;
        let encoded = MemoryImage::from_dense(&dense).encoded().to_vec();
        for offset in [8, 16, 24, 32, 40, 48, 56, 64, 72] {
            let mut bad = encoded.clone();
            bad[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            assert!(
                MemoryImage::from_storage(ImageStorage::Owned(bad), 100).is_err(),
                "overflowing sparse field at {offset} must be refused"
            );
        }
        let mut overlapping = encoded.clone();
        overlapping[56..64].copy_from_slice(&1u64.to_le_bytes());
        assert!(
            MemoryImage::from_storage(ImageStorage::Owned(overlapping), 100).is_err(),
            "overlapping regions must be refused"
        );
        assert!(
            MemoryImage::from_storage(
                ImageStorage::Owned(encoded[..encoded.len() - 1].to_vec()),
                100
            )
            .is_err(),
            "truncated payload must be refused"
        );
        assert!(
            MemoryImage::from_storage(ImageStorage::Owned(encoded), 99).is_err(),
            "configured memory bound must be enforced"
        );
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
