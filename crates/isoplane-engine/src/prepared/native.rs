//! Native code shared by every template whose checked local manifest records
//! the same compiled payload, so an engine holds one copy per distinct
//! runtime however many applications it serves.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Weak};

/// One deserialized native module. Templates hold it strongly and the
/// engine's index weakly, so it lives exactly as long as a template uses it.
pub(crate) struct SharedNative {
    pub(crate) module: wasmtime::Module,
}

/// Native modules this engine has deserialized, by the BLAKE3 digest of the
/// payload bytes they were verified and deserialized from.
#[derive(Default)]
pub(crate) struct NativeIndex(parking_lot::Mutex<HashMap<[u8; 32], Weak<SharedNative>>>);

impl NativeIndex {
    /// The module a live template already holds for `hash`.
    pub(crate) fn get(&self, hash: &[u8; 32]) -> Option<Arc<SharedNative>> {
        let mut modules = self.0.lock();
        let native = modules.get(hash).and_then(Weak::upgrade);
        if native.is_none() {
            modules.remove(hash);
        }
        native
    }

    /// Indexes a module deserialized from verified bytes that hashed to
    /// `hash`. When a concurrent load indexed the same payload first, returns
    /// that module and drops this one.
    pub(crate) fn insert(&self, hash: [u8; 32], module: wasmtime::Module) -> Arc<SharedNative> {
        let mut modules = self.0.lock();
        if let Some(native) = modules.get(&hash).and_then(Weak::upgrade) {
            return native;
        }
        modules.retain(|_, native| native.strong_count() > 0);
        let native = Arc::new(SharedNative { module });
        modules.insert(hash, Arc::downgrade(&native));
        native
    }
}

/// Native code copied into memory as it is verified, so the module the engine
/// maps is exactly the bytes it hashed and none of it pages in from storage:
/// a failed page-in of code cannot be contained to the guest running it.
#[cfg(target_os = "linux")]
pub(crate) struct NativeCopy(std::fs::File);

#[cfg(not(target_os = "linux"))]
pub(crate) struct NativeCopy(Vec<u8>);

#[cfg(target_os = "linux")]
impl NativeCopy {
    pub(crate) fn new() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: the NUL-terminated name is valid and the flags request a new owned descriptor.
        let descriptor = unsafe {
            libc::memfd_create(
                c"isoplane-native-code".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a new descriptor, transferred exactly once into File.
        Ok(Self(unsafe { std::fs::File::from_raw_fd(descriptor) }))
    }

    /// Seals the copy against every change, then maps it as a module.
    ///
    /// # Safety
    /// The copied bytes must be a module serialized for this engine by a
    /// trusted producer.
    pub(crate) unsafe fn deserialize(
        self,
        engine: &wasmtime::Engine,
    ) -> wasmtime::Result<wasmtime::Module> {
        self.seal()?;
        // SAFETY: the caller vouches for the bytes, and the seals keep them unchanged for the mapping.
        unsafe { wasmtime::Module::deserialize_open_file(engine, self.0) }
    }

    fn seal(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is live and these seals prevent mutations after the code is mapped.
        let result = unsafe {
            libc::fcntl(
                self.0.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
impl NativeCopy {
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self(Vec::new()))
    }

    /// Loads the copy as a module.
    ///
    /// # Safety
    /// The copied bytes must be a module serialized for this engine by a
    /// trusted producer.
    pub(crate) unsafe fn deserialize(
        self,
        engine: &wasmtime::Engine,
    ) -> wasmtime::Result<wasmtime::Module> {
        // SAFETY: the caller vouches for the bytes.
        unsafe { wasmtime::Module::deserialize(engine, &self.0) }
    }
}

impl Write for NativeCopy {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    fn a_native_copy_is_sealed_against_mutation_before_it_is_mapped() -> std::io::Result<()> {
        let mut copy = NativeCopy::new()?;
        copy.write_all(b"compiled code")?;
        copy.seal()?;
        assert!(
            copy.0.write_at(b"changed", 0).is_err(),
            "mapped native code must be immutable"
        );
        assert!(
            copy.0.set_len(0).is_err(),
            "mapped native code must not be truncated"
        );
        Ok(())
    }
}
