//! Test support any crate's tests can carry without building the engine.
#![forbid(unsafe_code)]

pub mod files;
pub mod mapped_backing;
pub mod prepared;
pub mod process;
pub mod published_files;

pub use files::{copy_tree, directories_under, link_files};
pub use prepared::{prepared_path, repository_root};
pub use process::{
    assert_success, forward_lines, recv_before, run_successfully, ChildGuard, ChildTest, TestChild,
};

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

/// Asserts that `directory` holds no entries, with `reason` naming what should
/// have reclaimed them.
pub fn assert_empty_directory(directory: &Path, reason: &str) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("list {}: {error}", directory.display()))
        .count();
    assert_eq!(entries, 0, "{reason}: {}", directory.display());
}

/// Polls `condition` every few milliseconds until it holds or `within` has
/// passed, and returns whether it held. The deadline fails a hung test rather
/// than letting it hang.
pub fn wait_for(mut condition: impl FnMut() -> bool, within: Duration) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < within {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    condition()
}

/// `bytes` escaped for a WAT data segment, as `(data (i32.const 0) "...")` takes them.
pub fn wat_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("\\{byte:02x}")).collect()
}

/// The bytes `text` spells in hex, two digits each. Fails the test on
/// anything else.
pub fn hex_bytes(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "{text:?} is not whole bytes in hex"
    );
    (0..text.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(&text[at..at + 2], 16)
                .unwrap_or_else(|error| panic!("{text:?} is not hex: {error}"))
        })
        .collect()
}

/// An environment variable set for as long as the guard lives. Dropping it,
/// including while a failed assertion unwinds, restores what the process
/// had. Bind the guard to a name: `let _ = ...` drops it at once.
#[must_use = "the variable reverts when the guard drops; bind it to a name for as long as it is needed"]
pub struct EnvironmentVariable {
    name: OsString,
    previous: Option<OsString>,
}

impl EnvironmentVariable {
    /// Sets `name` to `value` until the guard drops.
    pub fn set(name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let name = name.as_ref().to_os_string();
        let previous = std::env::var_os(&name);
        std::env::set_var(&name, value);
        Self { name, previous }
    }
}

impl Drop for EnvironmentVariable {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var(&self.name, previous),
            None => std::env::remove_var(&self.name),
        }
    }
}

#[cfg(unix)]
use std::fs::{File, Permissions};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::PathBuf;

/// Permissions set on a file or directory for as long as the guard lives.
/// Dropping it, including while a failed assertion unwinds, restores what
/// the path had, so a test leaves nothing it cannot delete.
///
/// The restore goes through a handle opened before the change, so it reaches
/// the path after a rename and after its directory stops being searchable.
/// Bind the guard to a name: `let _ = ...` drops it at once.
#[cfg(unix)]
#[must_use = "the permissions revert when the guard drops; bind it to a name for as long as they are needed"]
pub struct TemporaryPermissions {
    path: PathBuf,
    file: File,
    original_mode: u32,
}

#[cfg(unix)]
impl TemporaryPermissions {
    /// Sets `path`'s mode bits, as `chmod` would, until the guard drops.
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard is where tests change permissions; it restores them when dropped"
    )]
    pub fn mode(path: impl AsRef<Path>, mode: u32) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let original_mode = file.metadata()?.permissions().mode() & 0o7777;
        file.set_permissions(Permissions::from_mode(mode))?;
        Ok(Self {
            path,
            file,
            original_mode,
        })
    }
}

#[cfg(unix)]
impl Drop for TemporaryPermissions {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard is where tests change permissions; this is the restore"
    )]
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = self
            .file
            .set_permissions(Permissions::from_mode(self.original_mode))
        {
            let message = format!(
                "restoring mode {:o} of {}: {error}",
                self.original_mode,
                self.path.display()
            );
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::EnvironmentVariable;

    #[test]
    fn wat_bytes_escapes_every_byte_as_two_hex_digits() {
        assert_eq!(super::wat_bytes(&[0, 0x0a, 0xff]), "\\00\\0a\\ff");
    }

    #[test]
    fn hex_bytes_reads_two_digits_a_byte_and_rejects_anything_else() {
        assert_eq!(super::hex_bytes("000aFF"), [0, 0x0a, 0xff]);
        assert_eq!(super::hex_bytes(""), []);
        for bad in ["0", "zz"] {
            assert!(
                std::panic::catch_unwind(|| super::hex_bytes(bad)).is_err(),
                "{bad:?} must fail"
            );
        }
    }

    #[test]
    fn an_environment_variable_holds_while_the_guard_lives_and_reverts_when_it_drops() {
        let name = format!("ISOPLANE_TEST_SUPPORT_VARIABLE_{}", std::process::id());
        let guard = EnvironmentVariable::set(&name, "set");
        assert_eq!(std::env::var(&name).as_deref(), Ok("set"));
        {
            let _inner = EnvironmentVariable::set(&name, "replaced");
            assert_eq!(std::env::var(&name).as_deref(), Ok("replaced"));
        }
        assert_eq!(
            std::env::var(&name).as_deref(),
            Ok("set"),
            "an inner guard restores the outer value"
        );
        drop(guard);
        assert!(
            std::env::var_os(&name).is_none(),
            "the outer guard removes a variable the process did not have"
        );
    }

    #[test]
    fn an_empty_directory_passes_and_an_entry_fails() {
        let directory = tempfile::tempdir().expect("directory");
        super::assert_empty_directory(directory.path(), "nothing was written");
        std::fs::write(directory.path().join("entry"), b"").expect("write an entry");
        let failed = std::panic::catch_unwind(|| {
            super::assert_empty_directory(directory.path(), "an entry was written")
        });
        assert!(failed.is_err(), "an entry must fail the assertion");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::TemporaryPermissions;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("the path exists")
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn the_mode_holds_while_the_guard_lives_and_reverts_when_it_drops() {
        let directory = tempfile::tempdir().expect("directory");
        let file = directory.path().join("file");
        std::fs::write(&file, b"").expect("write the file");
        let before = mode_of(&file);
        assert_ne!(before, 0o400, "the fixture starts writable");
        let guard = TemporaryPermissions::mode(&file, 0o400).expect("set the mode");
        assert_eq!(
            mode_of(&file),
            0o400,
            "the mode holds while the guard lives"
        );
        drop(guard);
        assert_eq!(
            mode_of(&file),
            before,
            "the mode reverts when the guard drops"
        );
    }

    #[test]
    fn the_mode_reverts_while_a_failed_test_unwinds() {
        let directory = tempfile::tempdir().expect("directory");
        let sealed = directory.path().join("sealed");
        std::fs::create_dir(&sealed).expect("create the directory");
        let before = mode_of(&sealed);
        let unwound = std::panic::catch_unwind(|| {
            let _sealed = TemporaryPermissions::mode(&sealed, 0o500).expect("set the mode");
            assert_eq!(mode_of(&sealed), 0o500);
            panic!("a failed assertion");
        });
        assert!(unwound.is_err(), "the closure panicked");
        assert_eq!(
            mode_of(&sealed),
            before,
            "the mode reverts while the panic unwinds"
        );
    }

    #[test]
    fn a_renamed_path_still_reverts() {
        let directory = tempfile::tempdir().expect("directory");
        let original = directory.path().join("original");
        std::fs::create_dir(&original).expect("create the directory");
        let before = mode_of(&original);
        let guard = TemporaryPermissions::mode(&original, 0o500).expect("set the mode");
        let moved = directory.path().join("moved");
        std::fs::rename(&original, &moved).expect("rename the directory");
        assert_eq!(mode_of(&moved), 0o500, "the mode moved with the directory");
        drop(guard);
        assert_eq!(
            mode_of(&moved),
            before,
            "the mode reverts at the directory's new name"
        );
    }
}
