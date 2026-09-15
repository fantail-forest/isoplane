//! Fault injection into files owned by an individual test fixture.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn writable(path: &Path) -> File {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("make the owned fixture writable for fault injection");
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open the owned fixture for fault injection")
}

/// Changes payload bytes without changing the length to exercise hash verification.
pub fn tamper(path: &Path) {
    let mut file = writable(path);
    let mut byte = [0];
    file.read_exact(&mut byte).expect("read fixture byte");
    byte[0] ^= 1;
    file.seek(SeekFrom::Start(0)).expect("rewind fixture");
    file.write_all(&byte).expect("tamper fixture byte");
    file.sync_all().expect("persist fixture corruption");
}

/// Invalidates existing mappings so their next access faults in the supervised worker.
pub fn truncate(path: &Path) {
    writable(path)
        .set_len(0)
        .expect("truncate the owned mapped fixture");
}
