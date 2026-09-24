//! Bounded reads of the initialization working set from verified template files.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{PreparedError, Result, PAYLOAD_NAMES};

/// Retained payload files for activation prefetch. A payload is absent when
/// the engine already holds it resident through a shared native module.
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

    pub(crate) fn read_all(&self) -> Result<()> {
        let total_bytes: u64 = self
            .0
            .iter()
            .flatten()
            .flat_map(|p| &p.ranges)
            .map(|r| r.end - r.start)
            .sum();
        let _span = tracing::info_span!("template_prefetch", total_bytes, mode = "file_range_read")
            .entered();
        // Bounded scratch space warms the file cache without retaining a second template copy.
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
}

#[cfg(test)]
fn read_payload(file: &File, length_bytes: u64, scratch: &mut [u8]) -> io::Result<()> {
    read_range(file, 0..length_bytes, scratch)
}

fn read_range(file: &File, range: std::ops::Range<u64>, scratch: &mut [u8]) -> io::Result<()> {
    let mut offset_bytes = range.start;
    while offset_bytes < range.end {
        let count = (range.end - offset_bytes).min(scratch.len() as u64) as usize;
        #[cfg(unix)]
        let read = std::os::unix::fs::FileExt::read_at(file, &mut scratch[..count], offset_bytes);
        #[cfg(windows)]
        let read =
            std::os::windows::fs::FileExt::seek_read(file, &mut scratch[..count], offset_bytes);
        match read {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            Ok(count) => offset_bytes += count as u64,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};

    #[test]
    fn prefetch_reads_through_the_last_partial_chunk_without_moving_the_file_cursor(
    ) -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        file.write_all(b"abcdefghijklmnopq")?;
        file.rewind()?;
        let mut scratch = [0; 8];
        read_payload(&file, 17, &mut scratch)?;
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
        let file = tempfile::tempfile()?;
        let error = read_payload(&file, 1, &mut [0; 8])
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
        let mut file = tempfile::tempfile()?;
        file.write_all(b"abcdefghijklmnopq")?;
        file.rewind()?;
        let mut scratch = [b'!'; 8];
        read_range(&file, 3..7, &mut scratch)?;
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

    fn payloads() -> Payloads {
        let files: [File; 3] = std::array::from_fn(|_| {
            let mut file = tempfile::tempfile().expect("payload");
            file.write_all(b"abcdefghijklmnopq").expect("payload bytes");
            file
        });
        Payloads::new(files.each_ref().map(Some), Path::new("test-payloads"))
            .expect("retained payloads")
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
        assert!(
            matches!(
                payloads().select(&[native_range(0, 0)]),
                Err(PreparedError::Invalid(_))
            ),
            "an empty learned range must be refused"
        );
    }

    #[test]
    fn selection_refuses_overflowing_ranges() {
        assert!(
            matches!(
                payloads().select(&[native_range(u64::MAX, 1)]),
                Err(PreparedError::Invalid(_))
            ),
            "a learned range end must not overflow"
        );
    }

    #[test]
    fn selection_refuses_ranges_beyond_the_payload() {
        assert!(
            matches!(
                payloads().select(&[native_range(16, 2)]),
                Err(PreparedError::Invalid(_))
            ),
            "a learned range must fit the verified file"
        );
    }

    #[test]
    fn selection_refuses_overlaps_even_when_input_is_unordered() {
        assert!(
            matches!(
                payloads().select(&[native_range(5, 4), native_range(3, 4)]),
                Err(PreparedError::Invalid(_))
            ),
            "overlapping learned ranges must be refused regardless of input order"
        );
    }

    #[test]
    fn selection_accepts_adjacent_ranges_and_omits_unselected_bytes() {
        let selected = payloads()
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
            .read_all()
            .expect("prefetch must not read unselected payloads or suffix bytes");
    }
}
