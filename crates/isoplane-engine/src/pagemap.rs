//! Page-table state of this process's own mappings, from `/proc/self/pagemap`.

use std::io;
use std::ops::Range;

const PRESENT: u64 = 1 << 63;
const SWAPPED: u64 = 1 << 62;
const FILE_OR_SHARED: u64 = 1 << 61;

/// Resident pages of any kind.
pub(crate) fn present(entry: u64) -> bool {
    entry & PRESENT != 0
}

/// Pages this process holds a private copy of, resident or swapped. In a
/// private file mapping these are exactly the pages written since mapping;
/// every other page still reads from the file.
pub(crate) fn private_copy(entry: u64) -> bool {
    entry & SWAPPED != 0 || (entry & PRESENT != 0 && entry & FILE_OR_SHARED == 0)
}

/// Byte ranges of the mapping at `address` whose page entries satisfy `select`,
/// relative to `address` and clipped to `length_bytes`. Reading the entries
/// faults nothing in.
pub(crate) fn ranges(
    address: usize,
    length_bytes: usize,
    select: fn(u64) -> bool,
) -> io::Result<Vec<Range<u64>>> {
    use std::os::unix::fs::FileExt;
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
    let file = std::fs::File::open("/proc/self/pagemap")?;
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
            if select(u64::from_ne_bytes(*entry)) {
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
    use std::io::Write;

    #[test]
    fn a_private_file_mapping_reports_only_the_pages_written_to_it() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        file.write_all(&vec![1; 65536 * 4])?;
        // SAFETY: the test owns the unshared temporary file for the mapping's lifetime.
        let mut mapping = unsafe { memmap2::MmapOptions::new().map_copy(&file)? };
        let address = mapping.as_ptr() as usize;
        assert_eq!(mapping[0], 1, "reading faults in a file page");
        assert!(
            ranges(address, mapping.len(), private_copy)?.is_empty(),
            "a page that was only read still reads from the file"
        );
        mapping[65536 * 2 + 5] = 9;
        assert_eq!(
            ranges(address, mapping.len(), private_copy)?,
            vec![65536 * 2..65536 * 2 + 4096],
            "exactly the written page is a private copy"
        );
        Ok(())
    }
}
