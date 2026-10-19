//! Warming the page cache with the initialization working set of verified
//! template files, so a new instance's first faults on the mapped template
//! hit memory rather than storage.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{PreparedError, Result, PAYLOAD_NAMES};

/// Retained payload files for page-cache warming. A payload is absent when
/// the engine holds it in memory, as it does native code.
pub(crate) struct Payloads(Vec<Option<Payload>>);

struct Payload {
    ranges: Vec<std::ops::Range<u64>>,
    file: File,
    path: PathBuf,
    length_bytes: u64,
}

impl Payloads {
    pub(super) fn new(files: [Option<&File>; 3], directory: &Path) -> Result<Self> {
        let payloads = files
            .into_iter()
            .zip(PAYLOAD_NAMES)
            .map(|(file, name)| {
                let Some(file) = file else {
                    return Ok(None);
                };
                let path = directory.join(name);
                let file = file.try_clone().map_err(|source| PreparedError::Io {
                    operation: "retain prefetch file",
                    path: path.clone(),
                    source,
                })?;
                let length_bytes = file
                    .metadata()
                    .map_err(|source| PreparedError::Io {
                        operation: "size prefetch file",
                        path: path.clone(),
                        source,
                    })?
                    .len();
                Ok(Some(Payload {
                    ranges: Vec::new(),
                    file,
                    path,
                    length_bytes,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self(payloads))
    }

    /// Keeps the learned `ranges`, dropping those in a payload already held.
    pub(super) fn select(mut self, ranges: &[super::ReadAhead]) -> Result<Self> {
        for payload in self.0.iter_mut().flatten() {
            payload.ranges.clear();
        }
        for range in ranges {
            let Some(payload) = &mut self.0[range.payload.index()] else {
                continue;
            };
            let end = range
                .offset_bytes
                .checked_add(range.length_bytes)
                .ok_or(PreparedError::Invalid("learned prefetch overflow"))?;
            if range.length_bytes == 0 || end > payload.length_bytes {
                return Err(PreparedError::Invalid("learned prefetch exceeds payload"));
            }
            payload.ranges.push(range.offset_bytes..end);
        }
        for payload in self.0.iter_mut().flatten() {
            payload.ranges.sort_by_key(|range| range.start);
            if payload
                .ranges
                .windows(2)
                .any(|pair| pair[0].end > pair[1].start)
            {
                return Err(PreparedError::Invalid("overlapping learned prefetch"));
            }
        }
        Ok(self)
    }

    /// Asks the kernel to bring the learned ranges into the page cache. It
    /// reads ahead asynchronously and skips pages already resident, so warming
    /// a resident template costs microseconds and copies nothing.
    #[cfg(target_os = "linux")]
    pub(crate) fn warm(&self) -> Result<()> {
        use std::os::fd::AsRawFd;
        let _span = tracing::info_span!(
            "template_warm",
            total_bytes = self.total_bytes(),
            mode = "fadvise_willneed"
        )
        .entered();
        for payload in self.0.iter().flatten() {
            for range in &payload.ranges {
                let (Ok(offset), Ok(length)) = (
                    libc::off_t::try_from(range.start),
                    libc::off_t::try_from(range.end - range.start),
                ) else {
                    return Err(PreparedError::Invalid(
                        "learned prefetch exceeds platform limit",
                    ));
                };
                // SAFETY: the descriptor is retained and the range was checked
                // against the payload's recorded length in `select`.
                let result = unsafe {
                    libc::posix_fadvise(
                        payload.file.as_raw_fd(),
                        offset,
                        length,
                        libc::POSIX_FADV_WILLNEED,
                    )
                };
                if result != 0 {
                    return Err(PreparedError::Io {
                        operation: "advise payload read-ahead",
                        path: payload.path.clone(),
                        source: io::Error::from_raw_os_error(result),
                    });
                }
            }
        }
        Ok(())
    }

    /// Reads the learned ranges through a bounded scratch buffer, which warms
    /// the file cache on a platform with no read-ahead advice.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn warm(&self) -> Result<()> {
        let _span = tracing::info_span!(
            "template_warm",
            total_bytes = self.total_bytes(),
            mode = "file_range_read"
        )
        .entered();
        let mut scratch = vec![0; 64 * 1024];
        for payload in self.0.iter().flatten() {
            for range in &payload.ranges {
                read_range(&payload.file, range.clone(), &mut scratch).map_err(|source| {
                    PreparedError::Io {
                        operation: "prefetch complete payload",
                        path: payload.path.clone(),
                        source,
                    }
                })?;
            }
        }
        Ok(())
    }

    fn total_bytes(&self) -> u64 {
        self.0
            .iter()
            .flatten()
            .flat_map(|p| &p.ranges)
            .map(|r| r.end - r.start)
            .sum()
    }
}

#[cfg(test)]
fn read_payload(file: &File, length_bytes: u64, scratch: &mut [u8]) -> io::Result<()> {
    read_range(file, 0..length_bytes, scratch)
}

#[cfg(any(test, not(target_os = "linux")))]
fn read_range(file: &File, range: std::ops::Range<u64>, scratch: &mut [u8]) -> io::Result<()> {
    #[cfg(windows)]
    let reader = independent_reader(file)?;
    let mut offset_bytes = range.start;
    while offset_bytes < range.end {
        let count = (range.end - offset_bytes).min(scratch.len() as u64) as usize;
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(file, &mut scratch[..count], offset_bytes);
        #[cfg(windows)]
        let read =
            std::os::windows::fs::FileExt::seek_read(&reader, &mut scratch[..count], offset_bytes);
        match read {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => offset_bytes += count as u64,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn independent_reader(file: &File) -> io::Result<File> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        ReOpenFile, FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    // Windows positioned reads move the cursor. Reopening the verified handle
    // preserves file identity while giving prefetch its own cursor.
    // SAFETY: `file` keeps the original handle valid throughout this call.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ReOpenFile returned a new owned handle, transferred to File once.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    fn prefetch_reads_through_the_last_partial_chunk_without_moving_the_file_cursor(
    ) -> io::Result<()> {
        let mut temporary = tempfile::NamedTempFile::new()?;
        let file = temporary.as_file_mut();
        file.write_all(b"abcdefghijklmnopq")?;
        file.rewind()?;
        let mut scratch = [0; 8];
        read_payload(file, 17, &mut scratch)?;
        assert_eq!(
            scratch[0], b'q',
            "prefetch must read the final partial chunk"
        );
        assert_eq!(
            file.stream_position()?,
            0,
            "prefetch must not move a shared descriptor cursor"
        );
        Ok(())
    }

    #[test]
    fn prefetch_refuses_an_incomplete_payload() -> io::Result<()> {
        let temporary = tempfile::NamedTempFile::new()?;
        let file = temporary.as_file();
        let error = read_payload(file, 1, &mut [0; 8])
            .expect_err("a missing payload byte must fail prefetch");
        assert_eq!(
            error.kind(),
            io::ErrorKind::UnexpectedEof,
            "truncated payloads must fail before activation"
        );
        Ok(())
    }
    #[test]
    fn a_range_reads_only_its_selected_bytes_without_moving_the_cursor() -> io::Result<()> {
        let mut temporary = tempfile::NamedTempFile::new()?;
        let file = temporary.as_file_mut();
        file.write_all(b"abcdefghijklmnopq")?;
        file.rewind()?;
        let mut scratch = [b'!'; 8];
        read_range(file, 3..7, &mut scratch)?;
        assert_eq!(
            &scratch, b"defg!!!!",
            "range reads must respect both selected boundaries"
        );
        assert_eq!(
            file.stream_position()?,
            0,
            "range reads must preserve the shared cursor"
        );
        Ok(())
    }

    fn payloads() -> ([tempfile::NamedTempFile; 3], Payloads) {
        let files = std::array::from_fn(|_| {
            let mut file = tempfile::NamedTempFile::new().expect("payload");
            file.write_all(b"abcdefghijklmnopq").expect("payload bytes");
            file
        });
        let payloads = Payloads::new(
            files.each_ref().map(|file| Some(file.as_file())),
            Path::new("test-payloads"),
        )
        .expect("retained payloads");
        (files, payloads)
    }

    fn native_range(offset_bytes: u64, length_bytes: u64) -> super::super::ReadAhead {
        super::super::ReadAhead {
            payload: super::super::PreparedPayload::Native,
            offset_bytes,
            length_bytes,
        }
    }

    #[test]
    fn selection_refuses_empty_ranges() {
        let (_files, payloads) = payloads();
        assert!(
            matches!(
                payloads.select(&[native_range(0, 0)]),
                Err(PreparedError::Invalid(_))
            ),
            "an empty learned range must be refused"
        );
    }

    #[test]
    fn selection_refuses_overflowing_ranges() {
        let (_files, payloads) = payloads();
        assert!(
            matches!(
                payloads.select(&[native_range(u64::MAX, 1)]),
                Err(PreparedError::Invalid(_))
            ),
            "a learned range end must not overflow"
        );
    }

    #[test]
    fn selection_refuses_ranges_beyond_the_payload() {
        let (_files, payloads) = payloads();
        assert!(
            matches!(
                payloads.select(&[native_range(16, 2)]),
                Err(PreparedError::Invalid(_))
            ),
            "a learned range must fit the verified file"
        );
    }

    #[test]
    fn selection_refuses_overlaps_even_when_input_is_unordered() {
        let (_files, payloads) = payloads();
        assert!(
            matches!(
                payloads.select(&[native_range(5, 4), native_range(3, 4)]),
                Err(PreparedError::Invalid(_))
            ),
            "overlapping learned ranges must be refused regardless of input order"
        );
    }

    #[test]
    fn selection_accepts_adjacent_ranges_and_omits_unselected_bytes() {
        let (_files, payloads) = payloads();
        let selected = payloads
            .select(&[native_range(3, 1), native_range(1, 2)])
            .expect("adjacent ranges");
        assert_eq!(
            selected.0[0].as_ref().expect("native payload").ranges,
            vec![1..3, 3..4],
            "selected ranges must be ordered"
        );
        selected.0[0]
            .as_ref()
            .expect("native payload")
            .file
            .set_len(4)
            .expect("remove unselected suffix");
        for payload in selected.0[1..].iter().flatten() {
            payload.file.set_len(0).expect("remove unselected payload");
        }
        selected
            .warm()
            .expect("warming must not touch unselected payloads or suffix bytes");
    }
}
