//! Initialization working-set observation through present process page-table entries.

use std::io;
use std::ops::Range;

/// The ranges of a mapping that initialization left resident, which a layer
/// stores first so they can be read ahead together. Includes fault-around
/// pages; presence is a conservative observed set, not an access counter.
/// Empty on platforms without a page-presence query: a layer captured there
/// has no read-ahead order.
pub(super) fn present_ranges(address: usize, length_bytes: usize) -> io::Result<Vec<Range<u64>>> {
    #[cfg(target_os = "linux")]
    {
        crate::pagemap::ranges(address, length_bytes, crate::pagemap::present)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (address, length_bytes);
        Ok(Vec::new())
    }
}

#[cfg(all(test, target_os = "linux"))]
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
