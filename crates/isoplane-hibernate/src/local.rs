//! Same-process snapshots: the pages an instance changed, stored raw after a
//! small header in the instance's own reservation and read straight back into
//! its memory. Host resources stay in this process, so the header carries only
//! the binding, globals, tables and where each run of pages goes.
//!
//! The reservation holds the postcard header's length and CRC-32 as
//! little-endian `u32`s, the header, then the bytes of every run that is not
//! all zero, in run order. Nothing is flushed to storage: if the kernel fails
//! to write a page back and later evicts it, a read returns stale bytes without
//! an error, so the header carries a CRC-32 of the payload as well.

use std::io::{IoSlice, IoSliceMut};
use std::ops::Range;

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};

use isoplane_engine::{InstanceImage, MemoryImage};

use crate::delta::{Binding, PAGE};
use crate::snapshot::SnapshotReservation;
use crate::{wake_error, Result, WakeFallback};

/// The guest shape a reservation has to hold.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotShape {
    /// Maximum reachable linear-memory length, including growth.
    pub max_memory_bytes: u64,
    /// Exported mutable numeric globals; table mutations are rejected by the engine.
    pub mutable_globals: usize,
}

/// Bounds a snapshot in which every page changed and no two neighbouring
/// pages share a run.
pub fn size_bound(shape: SnapshotShape) -> Result<u64> {
    use isoplane_engine::snapshot::GlobalValue;
    use postcard::experimental::serialized_size;

    let pages = shape.max_memory_bytes.div_ceil(PAGE as u64);
    let header = Header {
        module_hash: [u8::MAX; 32],
        abi_rev: u32::MAX,
        image: InstanceImage {
            memory_pages: u32::MAX,
            ..InstanceImage::default()
        },
        runs: Vec::new(),
        payload_crc: u32::MAX,
    };
    let widest_run = Run {
        first_page: u32::MAX,
        pages: u32::MAX,
        zero: true,
    };
    let mut bytes = (PREFIX_BYTES + serialized_size(&header)?) as u64;
    // Measure with the real codec; replace each empty sequence prefix and add
    // worst-width elements without allocating a maximum-size guest image.
    for (count, element_bytes) in [
        (pages, serialized_size(&widest_run)?),
        (
            shape.mutable_globals as u64,
            serialized_size(&GlobalValue::I64(i64::MIN))?,
        ),
    ] {
        let prefix_growth = serialized_size(&count)? - serialized_size(&0usize)?;
        bytes = bytes
            .checked_add(prefix_growth as u64)
            .and_then(|n| n.checked_add(count.checked_mul(element_bytes as u64)?))
            .context("snapshot size arithmetic overflow")?;
    }
    pages
        .checked_mul(PAGE as u64)
        .and_then(|payload| bytes.checked_add(payload))
        .context("snapshot size arithmetic overflow")
}

/// What a save wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saved {
    /// Pages that differ from the template, zero pages included.
    pub changed_pages: u32,
    /// Of those, pages that are entirely zero and so cost no payload.
    pub zero_pages: u32,
    /// Bytes written to the reservation.
    pub bytes: u64,
}

/// Saves the pages of `memory` that differ from `template`, with `image`.
/// `changed` narrows the pages compared to byte ranges that may differ; with
/// `None`, every page is compared.
pub fn save(
    reservation: &mut dyn SnapshotReservation,
    binding: Binding,
    image: InstanceImage,
    memory: &[u8],
    template: &MemoryImage,
    changed: Option<&[Range<u64>]>,
) -> Result<Saved> {
    let runs =
        tracing::info_span!("delta_diff").in_scope(|| changed_runs(memory, template, changed))?;
    let mut payload = crc32fast::Hasher::new();
    for run in runs.iter().filter(|run| !run.zero) {
        payload.update(&memory[run.bytes()]);
    }
    let header = Header {
        module_hash: binding.module_hash.0,
        abi_rev: binding.abi_rev,
        image,
        runs,
        payload_crc: payload.finalize(),
    };
    let encoded = postcard::to_allocvec(&header).context("postcard-encode snapshot header")?;
    let mut prefix = [0; PREFIX_BYTES];
    prefix[..4].copy_from_slice(
        &u32::try_from(encoded.len())
            .context("snapshot header exceeds its length field")?
            .to_le_bytes(),
    );
    prefix[4..].copy_from_slice(&crc32fast::hash(&encoded).to_le_bytes());
    let mut parts = Vec::with_capacity(header.runs.len() + 2);
    parts.push(IoSlice::new(&prefix));
    parts.push(IoSlice::new(&encoded));
    let (mut changed_pages, mut zero_pages) = (0, 0);
    for run in &header.runs {
        changed_pages += run.pages;
        if run.zero {
            zero_pages += run.pages;
        } else {
            parts.push(IoSlice::new(&memory[run.bytes()]));
        }
    }
    let bytes = parts.iter().map(|part| part.len() as u64).sum();
    tracing::info_span!("delta_write", bytes).in_scope(|| reservation.save(&parts))?;
    Ok(Saved {
        changed_pages,
        zero_pages,
        bytes,
    })
}

/// A saved snapshot whose header has been read and checked.
pub struct Snapshot {
    header: Header,
    payload_offset: u64,
}

impl Snapshot {
    /// Reads the header. A snapshot bound to another module or ABI revision,
    /// or whose header fails its checksum, fails with its [`WakeFallback`].
    pub fn open(reservation: &mut dyn SnapshotReservation, expect: Binding) -> Result<Self> {
        let mut prefix = [0; PREFIX_BYTES];
        reservation.read_exact_at(0, &mut [IoSliceMut::new(&mut prefix)])?;
        let [l0, l1, l2, l3, c0, c1, c2, c3] = prefix;
        let length = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
        if length > reservation.reserved_bytes().0 {
            return Err(wake_error(WakeFallback::Corrupt));
        }
        let mut encoded = vec![0; length as usize];
        reservation.read_exact_at(PREFIX_BYTES as u64, &mut [IoSliceMut::new(&mut encoded)])?;
        if crc32fast::hash(&encoded) != u32::from_le_bytes([c0, c1, c2, c3]) {
            return Err(wake_error(WakeFallback::Corrupt));
        }
        let header: Header =
            postcard::from_bytes(&encoded).map_err(|_| wake_error(WakeFallback::Corrupt))?;
        if header.module_hash != expect.module_hash.0 {
            return Err(wake_error(WakeFallback::ModuleMismatch));
        }
        if header.abi_rev != expect.abi_rev {
            return Err(wake_error(WakeFallback::AbiMismatch));
        }
        Ok(Snapshot {
            header,
            payload_offset: PREFIX_BYTES as u64 + length,
        })
    }

    /// Globals, tables and the memory size to grow to before [`Snapshot::apply`].
    pub fn image(&self) -> &InstanceImage {
        &self.header.image
    }

    /// Writes the saved pages over `memory`, which holds the template's
    /// contents and has grown to the image's size. Pages that fail their
    /// checksum fail with [`WakeFallback::Corrupt`] after `memory` has been
    /// written, so the caller discards it.
    pub fn apply(
        &self,
        reservation: &mut dyn SnapshotReservation,
        mut memory: &mut [u8],
    ) -> Result<()> {
        let mut parts = Vec::with_capacity(self.header.runs.len());
        let mut consumed = 0;
        for run in &self.header.runs {
            let bytes = run.bytes();
            ensure!(
                bytes.start >= consumed && bytes.end - consumed <= memory.len(),
                "snapshot run at page {} is out of order or past the {}-byte memory",
                run.first_page,
                consumed + memory.len()
            );
            let (_, rest) = std::mem::take(&mut memory).split_at_mut(bytes.start - consumed);
            let (target, rest) = rest.split_at_mut(bytes.len());
            memory = rest;
            consumed = bytes.end;
            if run.zero {
                target.fill(0);
            } else {
                parts.push(IoSliceMut::new(target));
            }
        }
        reservation.read_exact_at(self.payload_offset, &mut parts)?;
        let mut payload = crc32fast::Hasher::new();
        for part in &parts {
            payload.update(part);
        }
        if payload.finalize() != self.header.payload_crc {
            return Err(wake_error(WakeFallback::Corrupt));
        }
        Ok(())
    }
}

/// Header length and header CRC-32.
const PREFIX_BYTES: usize = 8;

#[derive(Serialize, Deserialize)]
struct Header {
    module_hash: [u8; 32],
    abi_rev: u32,
    image: InstanceImage,
    /// Ascending and disjoint.
    runs: Vec<Run>,
    payload_crc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Run {
    first_page: u32,
    pages: u32,
    zero: bool,
}

impl Run {
    fn bytes(&self) -> Range<usize> {
        let start = self.first_page as usize * PAGE;
        start..start + self.pages as usize * PAGE
    }
}

/// Runs of pages that differ from the template, each either all zero or not.
fn changed_runs(
    memory: &[u8],
    template: &MemoryImage,
    changed: Option<&[Range<u64>]>,
) -> Result<Vec<Run>> {
    ensure!(
        memory.len().is_multiple_of(PAGE),
        "live memory is {} bytes, not a multiple of {PAGE}",
        memory.len()
    );
    let pages = memory.len() / PAGE;
    let everything = 0..memory.len() as u64;
    let mut runs: Vec<Run> = Vec::new();
    let mut next = 0;
    for range in changed.unwrap_or(std::slice::from_ref(&everything)) {
        let first = (range.start as usize / PAGE).max(next);
        let end = (range.end as usize).div_ceil(PAGE).min(pages);
        for page in first..end {
            let bytes = &memory[page * PAGE..(page + 1) * PAGE];
            if template.page_eq(page, bytes) {
                continue;
            }
            let zero = bytes.iter().all(|&byte| byte == 0);
            let page = u32::try_from(page).context("page index exceeds u32")?;
            match runs.last_mut() {
                Some(run) if run.zero == zero && run.first_page + run.pages == page => {
                    run.pages += 1
                }
                _ => runs.push(Run {
                    first_page: page,
                    pages: 1,
                    zero,
                }),
            }
        }
        next = next.max(end);
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{SnapshotBytes, SnapshotError};
    use isoplane_engine::snapshot::GlobalValue;
    use isoplane_engine::ModuleHash;

    /// An in-memory reservation that records how many parts each save wrote.
    #[derive(Default)]
    struct Memory {
        bytes: Vec<u8>,
        capacity: u64,
        saved_parts: Vec<usize>,
    }

    impl SnapshotReservation for Memory {
        fn reserved_bytes(&self) -> SnapshotBytes {
            SnapshotBytes(self.capacity)
        }
        fn save(&mut self, parts: &[IoSlice<'_>]) -> std::result::Result<(), SnapshotError> {
            self.bytes = parts.iter().flat_map(|part| part.iter().copied()).collect();
            assert!(
                self.bytes.len() as u64 <= self.capacity,
                "save exceeds the reservation"
            );
            self.saved_parts.push(parts.len());
            Ok(())
        }
        fn read_exact_at(
            &mut self,
            offset: u64,
            parts: &mut [IoSliceMut<'_>],
        ) -> std::result::Result<(), SnapshotError> {
            let mut at = offset as usize;
            for part in parts {
                let end = at + part.len();
                if end > self.bytes.len() {
                    return Err(SnapshotError::BeyondSnapshot {
                        end: end as u64,
                        saved: self.bytes.len() as u64,
                    });
                }
                part.copy_from_slice(&self.bytes[at..end]);
                at = end;
            }
            Ok(())
        }
    }

    fn binding() -> Binding {
        Binding::new(ModuleHash([3; 32]), 7)
    }

    fn template(pages: usize) -> Vec<u8> {
        (0..pages * PAGE)
            .map(|byte| (byte / PAGE) as u8 + 1)
            .collect()
    }

    fn reservation(pages: usize, globals: usize) -> Memory {
        Memory {
            capacity: size_bound(SnapshotShape {
                max_memory_bytes: (pages * PAGE) as u64,
                mutable_globals: globals,
            })
            .expect("bound a small guest"),
            ..Memory::default()
        }
    }

    #[test]
    fn changed_and_zeroed_pages_return_to_a_fresh_template_copy() {
        let base = template(8);
        let image = MemoryImage::from_dense(&base);
        let mut live = base.clone();
        live.resize(10 * PAGE, 0);
        live[PAGE..2 * PAGE].fill(0xaa);
        live[2 * PAGE + 9] = 0xbb;
        live[5 * PAGE..6 * PAGE].fill(0);
        live[9 * PAGE] = 0xcc;
        let globals = vec![GlobalValue::I64(-4), GlobalValue::F32(9)];
        let mut storage = reservation(10, globals.len());
        let saved = save(
            &mut storage,
            binding(),
            InstanceImage {
                globals: globals.clone(),
                table_mutations: Vec::new(),
                memory_pages: 1,
            },
            &live,
            &image,
            None,
        )
        .expect("save changed pages");
        assert_eq!(
            saved,
            Saved {
                changed_pages: 4,
                zero_pages: 1,
                bytes: saved.bytes,
            },
            "two neighbouring written pages, one zeroed page and one grown page changed"
        );
        assert_eq!(
            storage.saved_parts,
            vec![4],
            "the header and the two non-zero runs leave in one vectored save"
        );

        let snapshot = Snapshot::open(&mut storage, binding()).expect("open the snapshot");
        assert_eq!(snapshot.image().globals, globals);
        let mut restored = base.clone();
        restored.resize(10 * PAGE, 0);
        snapshot
            .apply(&mut storage, &mut restored)
            .expect("apply the snapshot");
        assert!(
            restored == live,
            "restored memory must equal the captured memory"
        );
    }

    #[test]
    fn only_the_changed_ranges_are_compared() {
        let base = template(4);
        let image = MemoryImage::from_dense(&base);
        let mut live = base.clone();
        live[PAGE] = 0xaa;
        live[3 * PAGE] = 0xbb;
        let mut storage = reservation(4, 0);
        let saved = save(
            &mut storage,
            binding(),
            InstanceImage::default(),
            &live,
            &image,
            Some(std::slice::from_ref(
                &((3 * PAGE) as u64..(4 * PAGE) as u64),
            )),
        )
        .expect("save the reported range");
        assert_eq!(
            saved.changed_pages, 1,
            "a page outside the changed ranges is taken to equal the template"
        );
    }

    #[test]
    fn a_snapshot_for_another_module_or_abi_falls_back() {
        let base = template(1);
        let mut storage = reservation(1, 0);
        save(
            &mut storage,
            binding(),
            InstanceImage::default(),
            &base,
            &MemoryImage::from_dense(&base),
            None,
        )
        .expect("save");
        for (expect, reason) in [
            (
                Binding::new(ModuleHash([4; 32]), 7),
                WakeFallback::ModuleMismatch,
            ),
            (
                Binding::new(ModuleHash([3; 32]), 8),
                WakeFallback::AbiMismatch,
            ),
        ] {
            let error = Snapshot::open(&mut storage, expect)
                .err()
                .expect("a mismatched binding must not open");
            assert_eq!(error.downcast_ref::<WakeFallback>(), Some(&reason));
        }
    }

    fn saved_with_a_changed_page() -> (Memory, Vec<u8>) {
        let base = template(2);
        let mut live = base.clone();
        live[PAGE + 7] = 0xaa;
        let mut storage = reservation(2, 0);
        save(
            &mut storage,
            binding(),
            InstanceImage::default(),
            &live,
            &MemoryImage::from_dense(&base),
            None,
        )
        .expect("save");
        (storage, base)
    }

    #[test]
    fn a_page_altered_after_saving_fails_its_checksum() {
        let (mut storage, base) = saved_with_a_changed_page();
        *storage.bytes.last_mut().expect("payload") ^= 1;
        let snapshot = Snapshot::open(&mut storage, binding()).expect("the header is intact");
        let error = snapshot
            .apply(&mut storage, &mut base.clone())
            .expect_err("altered pages must not be accepted");
        assert_eq!(
            error.downcast_ref::<WakeFallback>(),
            Some(&WakeFallback::Corrupt)
        );
    }

    #[test]
    fn a_header_altered_after_saving_fails_its_checksum() {
        let (mut storage, _) = saved_with_a_changed_page();
        storage.bytes[PREFIX_BYTES] ^= 1;
        let error = Snapshot::open(&mut storage, binding())
            .err()
            .expect("an altered header must not be used");
        assert_eq!(
            error.downcast_ref::<WakeFallback>(),
            Some(&WakeFallback::Corrupt)
        );
    }

    #[test]
    fn a_header_longer_than_the_reservation_is_corrupt() {
        let mut storage = Memory {
            bytes: [u32::MAX.to_le_bytes(), [0; 4]].concat(),
            capacity: 64,
            ..Memory::default()
        };
        let error = Snapshot::open(&mut storage, binding())
            .err()
            .expect("an impossible length must not be allocated");
        assert_eq!(
            error.downcast_ref::<WakeFallback>(),
            Some(&WakeFallback::Corrupt)
        );
    }

    #[test]
    fn every_page_changed_in_alternating_runs_fits_the_bound() {
        let pages = 64;
        let base = template(pages);
        let live: Vec<u8> = (0..pages * PAGE)
            .map(|byte| {
                if (byte / PAGE).is_multiple_of(2) {
                    0
                } else {
                    0xee
                }
            })
            .collect();
        let mut storage = reservation(pages, 128);
        let saved = save(
            &mut storage,
            binding(),
            InstanceImage {
                globals: vec![GlobalValue::I64(i64::MIN); 128],
                table_mutations: Vec::new(),
                memory_pages: u32::MAX,
            },
            &live,
            &MemoryImage::from_dense(&base),
            None,
        )
        .expect("the worst case must fit its reservation");
        assert_eq!(saved.changed_pages as usize, pages);
    }

    #[test]
    fn an_unrepresentable_shape_is_refused() {
        assert!(
            size_bound(SnapshotShape {
                max_memory_bytes: PAGE as u64,
                mutable_globals: usize::MAX,
            })
            .is_err(),
            "metadata multiplication must fail instead of wrapping the reservation"
        );
    }
}
