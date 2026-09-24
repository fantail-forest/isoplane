//! Initialization capture against an unchanged compiled module.

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use super::{
    files, state::State, Manifest, PreparedError, PreparedIdentity, PreparedPayload,
    PreparedStorage, PreparedTemplate, ReadAhead, Result, FORMAT, MAGIC,
};
use crate::{Instance, InstanceOps, IsoplaneEngine, Template};

impl IsoplaneEngine {
    /// Compile once, then map the unchanged code before observing initialization's working set.
    pub fn load_layer_source(&self, wasm: &[u8]) -> crate::Result<Template> {
        let source = self.load_template(wasm)?;
        let file = compiled_file(&source.inner.module().serialize()?)?;
        // SAFETY: this engine compiled these bytes; the memfd is sealed against changes.
        let module =
            unsafe { wasmtime::Module::deserialize_open_file(&self.inner.wasmtime, file) }?;
        self.finish_template(
            module,
            source.hash,
            source.inner.deferred.clone(),
            source.inner.prepared_resources.clone(),
            source
                .inner
                .mut_globals
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            source
                .inner
                .tables
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        )
    }

    /// Capture a completed initializer without recompiling its code. Refuses unsupported live state.
    /// The preparation caller must first instrument and admit the module, including all mutable
    /// exports and the restrictions on passive segments and function-table mutation.
    pub fn prepare_initialized_template(
        &self,
        template: &Template,
        instance: &mut Instance,
        identity: PreparedIdentity,
        resources: isoplane_abi::native_io::PreparedResources,
        deferred: Vec<u32>,
    ) -> Result<PreparedTemplate> {
        if !Arc::ptr_eq(&template.inner, &instance.template)
            || !instance.is_capture_eligible()
            || instance.last != Some(crate::Completion::Done)
        {
            return Err(PreparedError::Invalid(
                "layer requires its own successfully completed initializer",
            ));
        }
        let native_range = template.inner.module().image_range();
        let native_address = native_range.start as usize;
        let native_length = native_range.end as usize - native_address;
        let native_ranges = super::residency::present_ranges(native_address, native_length)
            .map_err(|error| PreparedError::Engine(error.into()))?;
        let memory = instance.memory();
        let memory_ranges =
            super::residency::present_ranges(memory.as_ptr() as usize, memory.len())
                .map_err(|error| PreparedError::Engine(error.into()))?;
        let mut prefetch: Vec<_> = native_ranges
            .into_iter()
            .map(|range| ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: range.start,
                length_bytes: range.end - range.start,
            })
            .collect();
        let captured = instance
            .capture_mutable_state()
            .map_err(PreparedError::Engine)?;
        let mut state = State::capture(template);
        state.memory_pages = captured.memory_pages;
        state.deferred = deferred;
        state.prepared_resources = resources;
        state.has_initialize = false;
        for ((_, value), captured_value) in state.mut_globals.iter_mut().zip(captured.globals) {
            *value = captured_value;
        }
        let image = crate::MemoryImage::from_layer(instance.memory(), &memory_ranges)
            .map_err(PreparedError::Invalid)?;
        let selected = image.layer_prefetch();
        if !selected.is_empty() {
            prefetch.push(ReadAhead {
                payload: PreparedPayload::Baseline,
                offset_bytes: selected.start,
                length_bytes: selected.end - selected.start,
            });
        }
        state.prefetch = prefetch;
        state.validate(&image, self.inner.cfg.max_memory_bytes)?;
        let payloads = [
            template
                .inner
                .module()
                .serialize()
                .map_err(|error| PreparedError::Engine(error.into()))?,
            image.encoded().to_vec(),
            postcard::to_allocvec(&state)?,
        ];
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.initialization-layer.v1\0");
        hash.update(&template.hash.0);
        hash.update(&postcard::to_allocvec(&identity)?);
        for bytes in &payloads {
            hash.update(blake3::hash(bytes).as_bytes());
        }
        Ok(PreparedTemplate {
            manifest: Manifest {
                magic: MAGIC,
                format: FORMAT,
                abi_rev: template.abi_rev,
                engine: self.inner.fingerprint.clone(),
                identity,
                module_hash: *hash.finalize().as_bytes(),
                files: std::array::from_fn(|index| files::FileRecord::new(&payloads[index])),
            },
            payloads,
        })
    }

    /// Loads captured preparation output without compilation.
    /// # Safety
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn load_prepared_product(&self, product: &PreparedTemplate) -> Result<Template> {
        let directory = tempfile::tempdir().map_err(|error| PreparedError::Engine(error.into()))?;
        let path = directory.path().join("prepared");
        product.publish(&path, PreparedStorage::VerifiedCopy)?;
        unsafe {
            // SAFETY: the caller guarantees controlled local compiler origin for this product; its backing remains owned and unchanged.
            self.load_prepared(&path, product.manifest.identity)
        }
    }
}

pub(super) fn from_file(
    file: File,
    image: &crate::MemoryImage,
) -> Result<wasmtime::CowMemoryImage> {
    // SAFETY: callers provide the same verified immutable descriptor used for admission;
    // the image retains it and its mapping through every allocator slot that references it.
    unsafe { wasmtime::CowMemoryImage::from_file_extents(file, image.len(), image.layer_extents()) }
        .map_err(|error| PreparedError::Engine(error.into()))
}

pub(super) fn from_owned(
    bytes: &[u8],
    image: &crate::MemoryImage,
) -> Result<wasmtime::CowMemoryImage> {
    let mut file = tempfile::tempfile().map_err(|error| PreparedError::Engine(error.into()))?;
    file.write_all(bytes)
        .map_err(|error| PreparedError::Engine(error.into()))?;
    from_file(file, image)
}

pub(super) fn attach(template: &mut Template, image: wasmtime::CowMemoryImage) -> Result<()> {
    Arc::get_mut(&mut template.inner)
        .ok_or(PreparedError::Invalid(
            "new layered template is already shared",
        ))?
        .initialization_layer = Some(image);
    Ok(())
}

fn compiled_file(bytes: &[u8]) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: the NUL-terminated name is valid and the flags request a new owned descriptor.
    let descriptor = unsafe {
        libc::memfd_create(
            c"isoplane-initialization-code".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a new descriptor, transferred exactly once into File.
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    file.write_all(bytes)?;
    // SAFETY: the descriptor is live and these seals prevent mutations after the code is mapped.
    let result = unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    fn compiled_source_is_sealed_against_mutation() -> std::io::Result<()> {
        let file = compiled_file(b"compiled source")?;
        assert!(
            file.write_at(b"changed", 0).is_err(),
            "mapped compiler output must be immutable"
        );
        assert!(
            file.set_len(0).is_err(),
            "mapped compiler output must not be truncated"
        );
        Ok(())
    }
}
