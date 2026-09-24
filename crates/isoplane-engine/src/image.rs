//! Sparse template memory with a persisted index and demand-readable payload.

use std::sync::Arc;

/// The unit hibernation diffs in.
pub const PAGE: usize = isoplane_abi::PAGE_SIZE;
const COALESCE_GAP: usize = 32;
const MAGIC: &[u8; 8] = b"ISOPSPR1";
const HEADER_BYTES: usize = 32;
const RUN_BYTES: usize = 24;
#[cfg(target_os = "linux")]
const LAYER_ALIGNMENT: usize = 65536;
#[cfg(target_os = "linux")]
const LAYER_MAGIC: &[u8; 8] = b"ISOPPKD1";

pub(crate) enum ImageStorage {
    Owned(Vec<u8>),
    /// A mapped baseline file. Its watch outlives the mapping, so a range is
    /// never watched after it is unmapped.
    Mapped {
        #[cfg(target_os = "linux")]
        watch: crate::storage_fault::Watch,
        bytes: memmap2::Mmap,
    },
}

impl ImageStorage {
    /// Maps a baseline whose failed page-ins read as zeros and mark it failed.
    pub(crate) fn mapped(bytes: memmap2::Mmap) -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        let watch = {
            let start = bytes.as_ptr() as usize;
            crate::storage_fault::Watch::new(
                start..start + bytes.len(),
                crate::storage_fault::Access::Read,
            )?
        };
        Ok(Self::Mapped {
            #[cfg(target_os = "linux")]
            watch,
            bytes,
        })
    }
}

impl AsRef<[u8]> for ImageStorage {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped { bytes, .. } => bytes,
        }
    }
}

/// Sparse memory shared by templates and instances; zero gaps allocate no payload.
/// The run index is decoded at construction, so after it only payload bytes
/// are read from storage.
#[derive(Clone)]
pub struct MemoryImage {
    len: usize,
    runs: Arc<[Run]>,
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
    #[cfg(target_os = "linux")]
    pub(crate) fn from_layer(
        dense: &[u8],
        selected: &[std::ops::Range<u64>],
    ) -> Result<Self, &'static str> {
        let mut hot = Vec::new();
        let mut cold = Vec::new();
        let mut end = 0;
        for range in selected {
            let start =
                usize::try_from(range.start).map_err(|_| "layer range exceeds address space")?;
            let next =
                usize::try_from(range.end).map_err(|_| "layer range exceeds address space")?;
            if start < end
                || next <= start
                || next > dense.len()
                || start % PAGE != 0
                || next % PAGE != 0
            {
                return Err("invalid selected layer range");
            }
            if end < start {
                cold.push(end..start);
            }
            hot.push(start..next);
            end = next;
        }
        if end < dense.len() {
            cold.push(end..dense.len());
        }
        let hot_bytes = hot.iter().map(|range| range.len()).sum::<usize>();
        let mut runs = Vec::new();
        let mut payload = Vec::with_capacity(dense.len());
        for range in hot.into_iter().chain(cold) {
            runs.push(Run {
                off: range.start,
                start: payload.len(),
                len: range.len(),
            });
            payload.extend_from_slice(&dense[range]);
        }
        runs.sort_by_key(|run| run.off);
        let header_bytes = HEADER_BYTES
            .checked_add(
                runs.len()
                    .checked_mul(RUN_BYTES)
                    .ok_or("layer index overflow")?,
            )
            .ok_or("layer index overflow")?;
        let payload_offset = header_bytes
            .checked_next_multiple_of(LAYER_ALIGNMENT)
            .ok_or("layer header overflow")?;
        let mut encoded = vec![0; payload_offset];
        encoded[..8].copy_from_slice(LAYER_MAGIC);
        for (offset, value) in [(8, dense.len()), (16, runs.len()), (24, hot_bytes)] {
            encoded[offset..offset + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        for (index, run) in runs.iter().enumerate() {
            for (field, value) in [run.off, run.start, run.len].into_iter().enumerate() {
                let offset = HEADER_BYTES + index * RUN_BYTES + field * 8;
                encoded[offset..offset + 8].copy_from_slice(&(value as u64).to_le_bytes());
            }
        }
        encoded.extend_from_slice(&payload);
        Self::from_storage(ImageStorage::Owned(encoded), dense.len())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn layer_prefetch(&self) -> std::ops::Range<u64> {
        let mut count = [0; 8];
        count.copy_from_slice(&self.encoded()[24..32]);
        let start = self.payload_offset as u64;
        start..start + u64::from_le_bytes(count)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn layer_extents(&self) -> Vec<wasmtime::CowMemoryExtent> {
        self.runs
            .iter()
            .map(|run| wasmtime::CowMemoryExtent {
                file_offset: (self.payload_offset + run.start) as u64,
                memory_offset: run.off,
                length: run.len,
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn is_layer(&self) -> bool {
        self.encoded().get(..8) == Some(LAYER_MAGIC.as_slice())
    }

    /// Iterates stored regions in logical address order, without expanding zero gaps.
    pub fn regions(&self) -> impl Iterator<Item = (usize, &[u8])> {
        self.runs.iter().map(|run| {
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
            payload_offset: HEADER_BYTES + runs.len() * RUN_BYTES,
            runs: runs.into(),
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
        #[cfg(target_os = "linux")]
        if bytes.get(..8) == Some(LAYER_MAGIC.as_slice()) {
            let len = number(bytes, 8)?;
            let run_count = number(bytes, 16)?;
            let hot_bytes = number(bytes, 24)?;
            let index_end = HEADER_BYTES
                .checked_add(
                    run_count
                        .checked_mul(RUN_BYTES)
                        .ok_or("layer index overflow")?,
                )
                .ok_or("layer index overflow")?;
            let payload_offset = index_end
                .checked_next_multiple_of(LAYER_ALIGNMENT)
                .ok_or("layer header overflow")?;
            if len == 0
                || len > max_memory_bytes
                || len % 65536 != 0
                || hot_bytes > len
                || hot_bytes % PAGE != 0
                || payload_offset.checked_add(len) != Some(bytes.len())
                || bytes
                    .get(index_end..payload_offset)
                    .is_none_or(|header| header.iter().any(|b| *b != 0))
            {
                return Err("invalid packed layer dimensions or header");
            }
            let mut logical_end = 0usize;
            let mut physical = Vec::new();
            let mut runs = Vec::with_capacity(run_count);
            for index in 0..run_count {
                let position = HEADER_BYTES + index * RUN_BYTES;
                let off = number(bytes, position)?;
                let start = number(bytes, position + 8)?;
                let size = number(bytes, position + 16)?;
                if off != logical_end
                    || off % PAGE != 0
                    || start % PAGE != 0
                    || size == 0
                    || size % PAGE != 0
                {
                    return Err("invalid packed layer extent alignment or coverage");
                }
                logical_end = off.checked_add(size).ok_or("layer memory overflow")?;
                let end = start.checked_add(size).ok_or("layer payload overflow")?;
                if logical_end > len || end > len {
                    return Err("packed layer extent exceeds bounds");
                }
                physical.push(start..end);
                runs.push(Run {
                    off,
                    start,
                    len: size,
                });
            }
            physical.sort_by_key(|range| range.start);
            let mut physical_end = 0;
            for range in physical {
                if range.start != physical_end {
                    return Err("packed layer payload overlaps or has gaps");
                }
                physical_end = range.end;
            }
            if logical_end != len || physical_end != len {
                return Err("packed layer does not cover captured memory");
            }
            return Ok(Self {
                len,
                runs: runs.into(),
                payload_offset,
                storage: Arc::new(storage),
            });
        }
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
        let mut runs = Vec::with_capacity(run_count);
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
            runs.push(Run {
                off,
                start,
                len: size,
            });
        }
        if payload_end != payload_bytes {
            return Err("sparse payload contains unindexed bytes");
        }
        Ok(Self {
            len,
            runs: runs.into(),
            payload_offset,
            storage: Arc::new(storage),
        })
    }

    /// Whether a read of this image's mapped storage failed, so a comparison
    /// or copy made from it since is wrong.
    pub fn storage_failed(&self) -> bool {
        match self.storage.as_ref() {
            ImageStorage::Owned(_) => false,
            #[cfg(target_os = "linux")]
            ImageStorage::Mapped { watch, .. } => watch.faulted(),
            #[cfg(not(target_os = "linux"))]
            ImageStorage::Mapped { .. } => false,
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
        self.runs.len()
    }

    /// Compares a live page with the baseline, treating grown memory as zero.
    pub fn page_eq(&self, page: usize, live: &[u8]) -> bool {
        let Some(off) = page.checked_mul(PAGE) else {
            return false;
        };
        let Some(end) = off.checked_add(live.len()) else {
            return false;
        };
        let low = self.runs.partition_point(|run| run.off + run.len <= off);
        let mut cursor = off;
        for run in &self.runs[low..] {
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
            .field("runs", &self.runs.len())
            .field("bytes", &self.encoded().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn packed_layer_round_trips_and_serves_the_hibernation_baseline() {
        let mut memory = vec![0; 65536];
        memory[4097] = 31;
        let image = super::MemoryImage::from_layer(&memory, &[]).expect("layer");
        let restored = super::MemoryImage::from_storage(
            super::ImageStorage::Owned(image.encoded().to_vec()),
            memory.len(),
        )
        .expect("valid dense layer");
        assert!(
            restored.is_layer(),
            "dense image must retain its mapping format"
        );
        assert_eq!(
            restored.regions().next().expect("dense region").1,
            memory,
            "layer storage must preserve every initialized byte"
        );
        assert!(
            restored.page_eq(1, &memory[4096..8192]),
            "hibernation must compare against initialized layer bytes"
        );
        memory[4097] = 32;
        assert!(
            !restored.page_eq(1, &memory[4096..8192]),
            "hibernation must notice request changes to initialized bytes"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn packed_layer_rejects_truncation_and_header_garbage() {
        let image = super::MemoryImage::from_layer(&vec![0; 65536], &[]).expect("layer");
        let mut bytes = image.encoded().to_vec();
        bytes.pop();
        assert!(
            super::MemoryImage::from_storage(super::ImageStorage::Owned(bytes), 65536).is_err(),
            "truncated layer must fail before mapping"
        );
        let mut bytes = image.encoded().to_vec();
        bytes[128] = 1;
        assert!(
            super::MemoryImage::from_storage(super::ImageStorage::Owned(bytes), 65536).is_err(),
            "unknown dense header data must be refused"
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn selected_pages_are_contiguous_on_disk_without_changing_logical_memory() {
        let mut memory = vec![0; 65536];
        for (index, page) in memory.chunks_mut(4096).enumerate() {
            page.fill(index as u8);
        }
        let image = super::MemoryImage::from_layer(&memory, &[4096..8192, 16384..20480])
            .expect("packed layer");
        let hot = image.layer_prefetch();
        assert_eq!(
            hot.end - hot.start,
            8192,
            "selected bytes must occupy one exact range"
        );
        let bytes = &image.encoded()[hot.start as usize..hot.end as usize];
        assert_eq!(
            &bytes[..4096],
            &memory[4096..8192],
            "first selected page must lead the payload"
        );
        assert_eq!(
            &bytes[4096..],
            &memory[16384..20480],
            "next selected page must immediately follow"
        );
        assert_eq!(
            image.to_dense(),
            memory,
            "all selected and unselected bytes must retain their guest addresses"
        );
        for page in 0..16 {
            assert!(
                image.page_eq(page, &memory[page * 4096..(page + 1) * 4096]),
                "hibernation must resolve each packed page correctly"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn packed_layer_refuses_aliased_or_missing_payload_ranges() {
        let image =
            super::MemoryImage::from_layer(&vec![0; 65536], std::slice::from_ref(&(4096..8192)))
                .expect("layer");
        for offset in [
            super::HEADER_BYTES,
            super::HEADER_BYTES + super::RUN_BYTES + 8,
        ] {
            let mut bytes = image.encoded().to_vec();
            bytes[offset..offset + 8].copy_from_slice(&4096u64.to_le_bytes());
            assert!(
                super::MemoryImage::from_storage(super::ImageStorage::Owned(bytes), 65536).is_err(),
                "overlapping logical or physical extents must fail before mapping"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_layer_ranges_must_be_ordered_aligned_and_bounded() {
        for ranges in [
            std::iter::once(1..4096).collect(),
            std::iter::once(0..1).collect(),
            std::iter::once(0..0).collect(),
            std::iter::once(0..69632).collect(),
            vec![4096..8192, 0..4096],
            vec![0..8192, 4096..12288],
        ] {
            assert!(
                super::MemoryImage::from_layer(&vec![0; 65536], &ranges).is_err(),
                "invalid selected pages must fail before capture"
            );
        }
    }

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
        let golden: &[u8] = b"ISOPSPR1\x03\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x09";
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
