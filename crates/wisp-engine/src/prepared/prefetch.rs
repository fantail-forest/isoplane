//! Full verified-file reads for the activation prefetch experiment.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{PreparedError, Result, PAYLOAD_NAMES};

pub(crate) struct Payloads(Vec<Payload>);

struct Payload {
    file: File,
    path: PathBuf,
    length_bytes: u64,
}

impl Payloads {
    pub(super) fn new(files: &[File; 3], directory: &Path) -> Result<Self> {
        let payloads = files
            .iter()
            .zip(PAYLOAD_NAMES)
            .map(|(file, name)| {
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
                Ok(Payload {
                    file,
                    path,
                    length_bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self(payloads))
    }

    pub(crate) fn read_all(&self) -> Result<()> {
        let total_bytes: u64 = self.0.iter().map(|p| p.length_bytes).sum();
        let _span = tracing::info_span!("template_prefetch", total_bytes, mode = "full_file_read")
            .entered();
        // Bounded scratch space warms the file cache without retaining a second template copy.
        let mut scratch = vec![0; 64 * 1024];
        for payload in &self.0 {
            read_payload(&payload.file, payload.length_bytes, &mut scratch).map_err(|source| {
                PreparedError::Io {
                    operation: "prefetch complete payload",
                    path: payload.path.clone(),
                    source,
                }
            })?;
        }
        Ok(())
    }
}

fn read_payload(file: &File, length_bytes: u64, scratch: &mut [u8]) -> io::Result<()> {
    let mut offset_bytes = 0;
    while offset_bytes < length_bytes {
        let count = (length_bytes - offset_bytes).min(scratch.len() as u64) as usize;
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
}
