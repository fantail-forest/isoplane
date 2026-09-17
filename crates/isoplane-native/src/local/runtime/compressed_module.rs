//! Reuse compressed portable bytes while verifying their exact uncompressed contents.

use std::io::{Read, Write};
use std::path::Path;

pub(super) fn size(directory: &Path, wasm: &[u8]) -> anyhow::Result<u64> {
    let path = directory.join("module.wasm.zst");
    match std::fs::read(&path) {
        Ok(compressed) if matches(&compressed, wasm) => return Ok(compressed.len() as u64),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let compressed = zstd::encode_all(wasm, 19)?;
    std::fs::create_dir_all(directory)?;
    let mut pending = tempfile::NamedTempFile::new_in(directory)?;
    pending.write_all(&compressed)?;
    pending.flush()?;
    pending.persist(path)?;
    Ok(compressed.len() as u64)
}

fn matches(compressed: &[u8], wasm: &[u8]) -> bool {
    let Ok(decoder) = zstd::Decoder::new(compressed) else {
        return false;
    };
    let mut decoded = Vec::new();
    decoder
        .take(wasm.len() as u64 + 1)
        .read_to_end(&mut decoded)
        .is_ok()
        && decoded == wasm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_bytes_reuse_the_verified_compressed_module() {
        let directory = tempfile::tempdir().unwrap();
        let wasm = b"portable module";
        let initial = size(directory.path(), wasm).unwrap();
        let path = directory.path().join("module.wasm.zst");
        let modified = path.metadata().unwrap().modified().unwrap();
        assert_eq!(
            size(directory.path(), wasm).unwrap(),
            initial,
            "unchanged bytes must retain their verified compressed size"
        );
        assert_eq!(
            path.metadata().unwrap().modified().unwrap(),
            modified,
            "a verified cache hit must not recompress or rewrite the module"
        );
    }

    #[test]
    fn changed_bytes_and_invalid_compressed_data_are_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("module.wasm.zst");
        let wasm = b"current module";
        for previous in [
            zstd::encode_all(&b"another module"[..], 19).unwrap(),
            b"damaged compression".to_vec(),
            zstd::encode_all(&b"current module plus excess bytes"[..], 19).unwrap(),
        ] {
            std::fs::write(&path, previous).unwrap();
            let measured = size(directory.path(), wasm).unwrap();
            let compressed = std::fs::read(&path).unwrap();
            assert!(
                matches(&compressed, wasm),
                "cached compression must represent exactly the current module"
            );
            assert_eq!(
                measured,
                compressed.len() as u64,
                "admission must count the actual compressed bytes"
            );
        }
    }
}
