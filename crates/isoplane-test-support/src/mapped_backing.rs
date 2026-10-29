//! Fault injection into published files a test owns, beneath the mappings
//! that read them.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::TemporaryPermissions;

/// Opens the published file for writing; the handle keeps that access after
/// the file's mode reverts.
fn writable(path: &Path) -> File {
    let _writable = TemporaryPermissions::mode(path, 0o600)
        .unwrap_or_else(|error| panic!("make {} writable: {error}", path.display()));
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("open {} for fault injection: {error}", path.display()))
}

/// Shrinks the file beneath its existing mappings, so their next access faults.
pub fn truncate(path: &Path) {
    writable(path)
        .set_len(0)
        .unwrap_or_else(|error| panic!("truncate {}: {error}", path.display()));
}

/// Writes `bytes` back into the file, as storage that recovers from a failure would.
pub fn restore(path: &Path, bytes: &[u8]) {
    writable(path)
        .write_all(bytes)
        .unwrap_or_else(|error| panic!("restore {}: {error}", path.display()));
}
