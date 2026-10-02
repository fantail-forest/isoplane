//! Initialization capture against an unchanged compiled module.

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use super::{
    files, state::State, Manifest, PreparedError, PreparedIdentity, PreparedPayload,
    PreparedTemplate, ReadAhead, Result, FORMAT, MAGIC,
};
use crate::{Instance, InstanceOps, IsoplaneEngine, Template};

impl IsoplaneEngine {
    /// Compile once, then map the unchanged code before observing initialization's working set.
    pub fn load_layer_source(&self, wasm: &[u8]) -> crate::Result<Template> {
        let source = self.load_template(wasm)?;
        let mut copy = super::native::NativeCopy::new()?;
        copy.write_all(&source.inner.module().serialize()?)?;
        // SAFETY: this engine compiled these bytes.
        let module = unsafe { copy.deserialize(&self.inner.wasmtime) }?;
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
        self.capture_layer(
            template,
            instance,
            identity,
            resources,
            deferred,
            Capture::Application,
        )
    }

    /// Capture a completed runtime-only initializer as a base: a template
    /// that still runs the application's initializer, and that application
    /// templates are captured as layers over.
    pub fn prepare_runtime_baseline(
        &self,
        template: &Template,
        instance: &mut Instance,
        identity: PreparedIdentity,
        resources: isoplane_abi::native_io::PreparedResources,
        deferred: Vec<u32>,
    ) -> Result<PreparedTemplate> {
        self.capture_layer(
            template,
            instance,
            identity,
            resources,
            deferred,
            Capture::RuntimeBaseline,
        )
    }

    /// Capture an application's completed initializer, run on an activation
    /// of `base`, as a layer over it: only memory that differs from `base` is
    /// stored, and the native code is `base`'s. `base` must be loaded from a
    /// published runtime baseline.
    pub fn prepare_initialized_template_over(
        &self,
        base: &Template,
        instance: &mut Instance,
        identity: PreparedIdentity,
        resources: isoplane_abi::native_io::PreparedResources,
        deferred: Vec<u32>,
    ) -> Result<PreparedTemplate> {
        self.capture_layer(base, instance, identity, resources, deferred, Capture::Over)
    }

    fn capture_layer(
        &self,
        template: &Template,
        instance: &mut Instance,
        identity: PreparedIdentity,
        resources: isoplane_abi::native_io::PreparedResources,
        deferred: Vec<u32>,
        capture: Capture,
    ) -> Result<PreparedTemplate> {
        if !Arc::ptr_eq(&template.inner, &instance.template)
            || !instance.is_capture_eligible()
            || instance.last != Some(crate::Completion::Done)
        {
            return Err(PreparedError::Invalid(
                "layer requires its own successfully completed initializer",
            ));
        }
        let base = match capture {
            Capture::Over => Some(template.inner.prepared_hash.ok_or(PreparedError::Invalid(
                "a base template must be loaded from a published baseline",
            ))?),
            Capture::Application | Capture::RuntimeBaseline => None,
        };
        let memory = instance.memory();
        let memory_ranges =
            super::residency::present_ranges(memory.as_ptr() as usize, memory.len())
                .map_err(|error| PreparedError::Engine(error.into()))?;
        let mut prefetch = Vec::new();
        if base.is_none() {
            let native_range = template.inner.module().image_range();
            let native_address = native_range.start as usize;
            let native_length = native_range.end as usize - native_address;
            let native_ranges = super::residency::present_ranges(native_address, native_length)
                .map_err(|error| PreparedError::Engine(error.into()))?;
            prefetch.extend(native_ranges.into_iter().map(|range| ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: range.start,
                length_bytes: range.end - range.start,
            }));
        }
        let captured = instance
            .capture_mutable_state()
            .map_err(PreparedError::Engine)?;
        let mut state = State::capture(template);
        state.memory_pages = captured.memory_pages;
        state.deferred = deferred;
        state.prepared_resources = resources;
        state.has_initialize = capture == Capture::RuntimeBaseline;
        for ((_, value), captured_value) in state.mut_globals.iter_mut().zip(captured.globals) {
            *value = captured_value;
        }
        let image = match capture {
            Capture::Over => crate::MemoryImage::from_overlay(
                instance.memory(),
                &template.inner.image,
                &memory_ranges,
                self.inner.cfg.max_memory_bytes as usize,
            ),
            Capture::Application | Capture::RuntimeBaseline => {
                crate::MemoryImage::from_layer(instance.memory(), &memory_ranges)
            }
        }
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
        let native = match base {
            Some(_) => Vec::new(),
            None => template
                .inner
                .module()
                .serialize()
                .map_err(|error| PreparedError::Engine(error.into()))?,
        };
        let payloads = [
            native,
            image.encoded().to_vec(),
            postcard::to_allocvec(&state)?,
        ];
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.initialization-layer.v1\0");
        hash.update(&template.hash.0);
        hash.update(&postcard::to_allocvec(&identity)?);
        hash.update(&postcard::to_allocvec(&base)?);
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
                base,
            },
            payloads,
        })
    }

    /// As [`IsoplaneEngine::load_prepared_product`], for a template captured as a layer over `base`.
    /// # Safety
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn load_prepared_product_over(
        &self,
        base: &Template,
        product: &PreparedTemplate,
    ) -> Result<Template> {
        unsafe {
            // SAFETY: the caller guarantees controlled local compiler origin for this product; its backing remains owned and unchanged.
            self.load_prepared_product_over_in(base, product, &std::env::temp_dir())
        }
    }

    /// Restore a layer using temporary backing files within `storage`.
    /// # Safety
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn load_prepared_product_over_in(
        &self,
        base: &Template,
        product: &PreparedTemplate,
        storage: &std::path::Path,
    ) -> Result<Template> {
        let directory =
            tempfile::tempdir_in(storage).map_err(|error| PreparedError::Engine(error.into()))?;
        let path = directory.path().join("prepared");
        product.publish_discarded(&path)?;
        // SAFETY: as for `load_prepared_product`; `base` retains its own files.
        unsafe { self.load_prepared_over(base, &path, product.manifest.identity, &[]) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// The whole initialization, as a complete layer.
    Application,
    /// The runtime-only initialization, as a complete layer that still runs
    /// the application's initializer.
    RuntimeBaseline,
    /// The application's initialization over a loaded runtime baseline, as
    /// a layer over it.
    Over,
}

/// Maps a layer's own baseline file and, for a layer over a base, the base's.
/// Returns the layer's own source, for layers captured over this one.
pub(super) fn from_file(
    file: File,
    image: &crate::MemoryImage,
    base: Option<&wasmtime::CowMemorySource>,
) -> Result<(wasmtime::CowMemorySource, wasmtime::CowMemoryImage)> {
    // SAFETY: callers provide the same verified immutable descriptor used for admission;
    // the source retains it and its mapping through every image and allocator slot that
    // references it.
    let own = unsafe { wasmtime::CowMemorySource::from_file(file) }
        .map_err(|error| PreparedError::Engine(error.into()))?;
    let (sources, extents) = match base {
        Some(base) => (vec![base.clone(), own.clone()], image.layer_extents(1)),
        None => (vec![own.clone()], image.layer_extents(0)),
    };
    let mapped = wasmtime::CowMemoryImage::from_sources(&sources, image.len(), extents)
        .map_err(|error| PreparedError::Engine(error.into()))?;
    Ok((own, mapped))
}

pub(super) fn attach(
    template: &mut Template,
    (source, image): (wasmtime::CowMemorySource, wasmtime::CowMemoryImage),
) -> Result<()> {
    let inner = Arc::get_mut(&mut template.inner).ok_or(PreparedError::Invalid(
        "new layered template is already shared",
    ))?;
    inner.initialization_layer = Some(image);
    inner.layer_source = Some(source);
    Ok(())
}
