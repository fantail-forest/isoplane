//! Initialization working-set observation through present process page-table entries.

use std::fs::File;
use std::io;
use std::ops::Range;
use std::os::unix::fs::FileExt;

/// Includes fault-around pages; presence is a conservative observed set, not an access counter.
pub(super) fn present_ranges(address: usize, length_bytes: usize) -> io::Result<Vec<Range<u64>>> {
    // SAFETY: sysconf reads the process's host page size and takes no pointer arguments.
    let page_bytes = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_bytes = usize::try_from(page_bytes)
        .ok()
        .filter(|value| value.is_power_of_two())
        .ok_or_else(|| io::Error::other("invalid host page size"))?;
    if !address.is_multiple_of(page_bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unaligned observed mapping",
        ));
    }
    let first_page = address / page_bytes;
    let pages = length_bytes.div_ceil(page_bytes);
    let file = File::open("/proc/self/pagemap")?;
    let mut ranges: Vec<Range<u64>> = Vec::new();
    let mut entries = [0; 8192];
    for first in (0..pages).step_by(entries.len() / 8) {
        let count = (pages - first).min(entries.len() / 8);
        let offset = first_page
            .checked_add(first)
            .and_then(|n| n.checked_mul(8))
            .ok_or_else(|| io::Error::other("pagemap offset overflow"))?;
        file.read_exact_at(&mut entries[..count * 8], offset as u64)?;
        for (index, entry) in entries[..count * 8].as_chunks::<8>().0.iter().enumerate() {
            if u64::from_ne_bytes(*entry) & (1 << 63) != 0 {
                let start = ((first + index) * page_bytes) as u64;
                let end = (start + page_bytes as u64).min(length_bytes as u64);
                match ranges.last_mut() {
                    Some(last) if last.end == start => last.end = end,
                    _ => ranges.push(start..end),
                }
            }
        }
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_a_written_page_without_faulting_untouched_pages() -> io::Result<()> {
        let mut mapping = memmap2::MmapMut::map_anon(65536 * 4)?;
        assert!(
            present_ranges(mapping.as_ptr() as usize, mapping.len())?.is_empty(),
            "a fresh anonymous mapping must have no present pages"
        );
        mapping[65536] = 7;
        let ranges = present_ranges(mapping.as_ptr() as usize, mapping.len())?;
        assert!(
            ranges.iter().any(|range| range.contains(&65536)),
            "observation must include the written page"
        );
        assert!(
            ranges
                .iter()
                .map(|range| range.end - range.start)
                .sum::<u64>()
                < mapping.len() as u64,
            "observation must not populate the rest of the mapping"
        );
        Ok(())
    }

    #[test]
    fn refuses_an_unaligned_mapping() {
        assert!(
            present_ranges(1, 65536).is_err(),
            "mapping offsets must be page aligned"
        );
    }
}
