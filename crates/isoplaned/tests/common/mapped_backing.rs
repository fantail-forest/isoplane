//! Fault injection into files owned by an individual test fixture.

use std::fs::{File, OpenOptions};
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

/// Shrinks the file beneath its existing mappings, so their next access faults.
pub fn truncate(path: &Path) {
    writable(path)
        .set_len(0)
        .expect("truncate the owned mapped fixture");
}
