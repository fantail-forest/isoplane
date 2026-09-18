use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

pub(super) fn evict(path: &Path) -> anyhow::Result<()> {
    let file = File::open(path)?;
    file.sync_all()?;
    // SAFETY: file owns the valid descriptor for the duration of this advisory call.
    let error = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    anyhow::ensure!(
        error == 0,
        "eviction advice failed: {}",
        std::io::Error::from_raw_os_error(error)
    );
    Ok(())
}

pub(super) fn resident_pages(path: &Path) -> anyhow::Result<(usize, usize)> {
    let file = File::open(path)?;
    let length = usize::try_from(file.metadata()?.len())?;
    if length == 0 {
        return Ok((0, 0));
    }
    // SAFETY: sysconf has no pointer arguments and _SC_PAGESIZE is supported on Linux.
    let page_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })?;
    anyhow::ensure!(page_size > 0, "kernel reported zero page size");
    let mut pages = vec![0_u8; length.div_ceil(page_size)];
    // SAFETY: mapping is used only as an address range for mincore; no file bytes are
    // dereferenced. The live file descriptor and exact nonzero length establish the range.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_NONE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    anyhow::ensure!(
        mapping != libc::MAP_FAILED,
        "cache probe mapping failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: mmap supplied a page-aligned valid range; pages has one byte per page.
    let result = unsafe { libc::mincore(mapping, length, pages.as_mut_ptr()) };
    let error = std::io::Error::last_os_error();
    // SAFETY: this is exactly the owned mapping above, never handed to another consumer.
    let unmap = unsafe { libc::munmap(mapping, length) };
    anyhow::ensure!(result == 0, "cache residency probe failed: {error}");
    anyhow::ensure!(
        unmap == 0,
        "cache probe unmap failed: {}",
        std::io::Error::last_os_error()
    );
    Ok((
        pages.iter().filter(|page| **page & 1 != 0).count(),
        pages.len(),
    ))
}
