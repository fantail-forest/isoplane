//! Locally compiled prepared templates, independent of artifact acquisition and build orchestration.

mod files;
mod layer;
pub(crate) mod native;
pub(crate) mod prefetch;
mod residency;
mod state;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{IsoplaneEngine, Template};

pub use files::{PreparedPayload, ReadAhead};

const FORMAT: u32 = 8;
const MAGIC: [u8; 8] = *b"ISOPPREP";
const NATIVE: &str = "template.cwasm";
const BASELINE: &str = "baseline.bin";
const STATE: &str = "state.bin";
const MANIFEST: &str = "manifest.bin";
const PAYLOAD_NAMES: [&str; 3] = [NATIVE, BASELINE, STATE];

/// Trusted input and toolchain identities supplied by preparation and expected by execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedIdentity {
    /// Hash of the input admitted by the trusted preparation caller.
    pub input_hash: [u8; 32],
    /// Digest of the exact runtime/toolchain profile, established outside guest input.
    pub runtime_profile: [u8; 32],
    /// Digest of declared initialization inputs and policy.
    pub initialization_hash: [u8; 32],
}

/// A prepared artifact failed admission or materialization before activation.
#[derive(Debug, thiserror::Error)]
pub enum PreparedError {
    /// A filesystem operation failed.
    #[error("prepared artifact {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Metadata has an unsupported or malformed encoding.
    #[error("prepared artifact encoding: {0}")]
    Encoding(#[from] postcard::Error),
    /// A compatibility identity did not match this worker or requested application.
    #[error("prepared artifact incompatible {0}")]
    Incompatible(&'static str),
    /// A payload or explicit state violates its recorded representation.
    #[error("invalid prepared artifact: {0}")]
    Invalid(&'static str),
    /// The compiler or native deserializer refused the prepared module.
    #[error("prepared template engine operation: {0:#}")]
    Engine(#[source] anyhow::Error),
}

type Result<T> = std::result::Result<T, PreparedError>;

#[derive(Serialize, Deserialize)]
struct Manifest {
    magic: [u8; 8],
    format: u32,
    abi_rev: u32,
    engine: String,
    identity: PreparedIdentity,
    module_hash: [u8; 32],
    files: [files::FileRecord; 3],
    /// The module hash of the template this one is a layer over; its native
    /// code and unchanged memory come from that template.
    base: Option<[u8; 32]>,
}

/// Fully prepared native code and explicit initial state, awaiting atomic publication.
///
/// Only engine capture or the unsafe trusted-builder decoder can establish native-code provenance.
/// General deserialization cannot create a trusted product:
///
/// ```compile_fail,E0277
/// use isoplane_engine::prepared::PreparedTemplate;
///
/// fn deserialize_untrusted(bytes: &[u8]) -> Result<PreparedTemplate, postcard::Error> {
///     postcard::from_bytes(bytes)
/// }
/// ```
#[derive(Serialize)]
#[serde(transparent)]
pub struct PreparedTemplate {
    data: PreparedTemplateData,
}

#[derive(Serialize, Deserialize)]
struct PreparedTemplateData {
    manifest: Manifest,
    #[serde(with = "byte_payloads")]
    payloads: [Vec<u8>; 3],
}

/// The payloads as byte strings, which postcard copies whole instead of element by element;
/// the encoding is the same as three sequences of bytes.
mod byte_payloads {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        payloads: &[Vec<u8>; 3],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let [native, image, state] = payloads;
        (
            serde_bytes::Bytes::new(native),
            serde_bytes::Bytes::new(image),
            serde_bytes::Bytes::new(state),
        )
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[Vec<u8>; 3], D::Error> {
        let (native, image, state) = <(
            serde_bytes::ByteBuf,
            serde_bytes::ByteBuf,
            serde_bytes::ByteBuf,
        )>::deserialize(deserializer)?;
        Ok([native.into_vec(), image.into_vec(), state.into_vec()])
    }
}

impl IsoplaneEngine {
    /// Compiles already initialized portable WASM and captures its complete baseline once.
    /// Called by controlled local preparation before activation.
    pub fn prepare_template(
        &self,
        template_wasm: &[u8],
        identity: PreparedIdentity,
    ) -> Result<PreparedTemplate> {
        self.prepare_template_with_files(template_wasm, identity, crate::PlatformFiles::default())
    }

    /// Prepare native state with immutable application files recorded in the artifact.
    pub fn prepare_template_with_files(
        &self,
        template_wasm: &[u8],
        identity: PreparedIdentity,
        application_files: crate::PlatformFiles,
    ) -> Result<PreparedTemplate> {
        self.inner
            .cfg
            .platform_files
            .merge(&application_files)
            .map_err(|error| PreparedError::Engine(error.into()))?;
        for payload in wasmparser::Parser::new(0).parse_all(template_wasm) {
            if matches!(
                payload.map_err(|error| PreparedError::Engine(error.into()))?,
                wasmparser::Payload::StartSection { .. }
            ) {
                return Err(PreparedError::Invalid(
                    "prepared portable input still has a start function",
                ));
            }
        }
        let template = self
            .load_template(template_wasm)
            .and_then(|template| template.with_application_files(application_files))
            .map_err(PreparedError::Engine)?;
        let native = template
            .inner
            .module()
            .serialize()
            .map_err(|e| PreparedError::Engine(e.into()))?;
        let state = state::State::capture(&template);
        let payloads = [
            native,
            template.inner.image.encoded().to_vec(),
            postcard::to_allocvec(&state)?,
        ];
        Ok(PreparedTemplate {
            data: PreparedTemplateData {
                manifest: Manifest {
                    magic: MAGIC,
                    format: FORMAT,
                    abi_rev: isoplane_abi::ABI_REV,
                    engine: self.inner.fingerprint.clone(),
                    identity,
                    module_hash: template.hash.0,
                    files: std::array::from_fn(|index| files::FileRecord::new(&payloads[index])),
                    base: None,
                },
                payloads,
            },
        })
    }

    /// Decodes successful output from the host's controlled local preparation compiler.
    ///
    /// # Safety
    /// The bytes must come exclusively from the controlled local compiler selected by the host.
    /// Customer-supplied native output is never admissible; checksums do not establish its origin.
    pub unsafe fn decode_builder_output(
        &self,
        bytes: &[u8],
        expected: PreparedIdentity,
    ) -> Result<PreparedTemplate> {
        // SAFETY: the caller guarantees these bytes originate exclusively from the trusted builder.
        unsafe { self.decode_prepared(bytes, expected, None) }
    }

    /// As [`IsoplaneEngine::decode_builder_output`], for output captured as a
    /// layer over `base`.
    ///
    /// # Safety
    /// As for [`IsoplaneEngine::decode_builder_output`].
    pub unsafe fn decode_builder_output_over(
        &self,
        base: &Template,
        bytes: &[u8],
        expected: PreparedIdentity,
    ) -> Result<PreparedTemplate> {
        // SAFETY: the caller guarantees these bytes originate exclusively from the trusted builder.
        unsafe { self.decode_prepared(bytes, expected, Some(base)) }
    }

    /// # Safety
    /// The bytes have the trusted-builder provenance required by `decode_builder_output`.
    unsafe fn decode_prepared(
        &self,
        bytes: &[u8],
        expected: PreparedIdentity,
        base: Option<&Template>,
    ) -> Result<PreparedTemplate> {
        let prepared: PreparedTemplateData = postcard::from_bytes(bytes)?;
        self.check_prepared(&prepared.manifest, expected)?;
        let base = matching_base(&prepared.manifest, base)?;
        prepared.verify_payloads()?;
        let image = crate::MemoryImage::from_storage_over(
            crate::image::ImageStorage::Owned(prepared.payloads[1].clone()),
            base.map(|base| std::sync::Arc::clone(&base.inner.image)),
            self.inner.cfg.max_memory_bytes as usize,
        )
        .map_err(PreparedError::Invalid)?;
        let state: state::State = postcard::from_bytes(&prepared.payloads[2])?;
        state.validate(&image, self.inner.cfg.max_memory_bytes)?;
        state.filesystem(&self.inner.cfg.platform_files)?;
        Ok(PreparedTemplate { data: prepared })
    }

    /// Loads captured preparation output without compilation.
    pub fn load_prepared_product(&self, product: &PreparedTemplate) -> Result<Template> {
        self.load_prepared_product_in(product, &std::env::temp_dir())
    }

    /// Restore preparation output, using temporary backing files within
    /// `storage` where the platform maps templates from files.
    pub fn load_prepared_product_in(
        &self,
        product: &PreparedTemplate,
        storage: &std::path::Path,
    ) -> Result<Template> {
        self.load_product(product, None, storage)
    }

    /// Loads `product`, over `base` when it is a layer over one. Linux maps a
    /// template copy-on-write from files, which it keeps in a directory under
    /// `storage` and unlinks at once.
    #[cfg(target_os = "linux")]
    fn load_product(
        &self,
        product: &PreparedTemplate,
        base: Option<&Template>,
        storage: &Path,
    ) -> Result<Template> {
        let directory =
            tempfile::tempdir_in(storage).map_err(|error| PreparedError::Engine(error.into()))?;
        let path = directory.path().join("prepared");
        product.publish_discarded(&path)?;
        let manifest = files::read_manifest_receipt(&path)?.0;
        self.check_prepared(&manifest, product.data.manifest.identity)?;
        // SAFETY: the directory is private to this call and nothing writes its published files;
        // removing it with the call's end unlinks them, which leaves the mappings intact. `base`
        // retains its own files. PreparedTemplate admits native code only from engine capture
        // or the unsafe decoder of output from the controlled local compiler.
        unsafe { self.map_prepared(&path, manifest, &[], base) }
    }

    /// Loads `product`, over `base` when it is a layer over one. Platforms
    /// other than Linux copy a template into each activation, so the template
    /// is held in memory and nothing is written to `_storage`: a file it was
    /// mapped from could not be removed on Windows while the template lived,
    /// and would be left behind by a session that was killed.
    #[cfg(not(target_os = "linux"))]
    fn load_product(
        &self,
        product: &PreparedTemplate,
        base: Option<&Template>,
        _storage: &Path,
    ) -> Result<Template> {
        use std::io::Write;
        let manifest = &product.data.manifest;
        self.check_prepared(manifest, manifest.identity)?;
        let base = matching_base(manifest, base)?;
        product.data.verify_payloads()?;
        let [native, baseline, state] = &product.data.payloads;
        let image = crate::MemoryImage::from_storage_over(
            crate::image::ImageStorage::Owned(baseline.clone()),
            base.map(|base| base.inner.image.clone()),
            self.inner.cfg.max_memory_bytes as usize,
        )
        .map_err(PreparedError::Invalid)?;
        let state: state::State = postcard::from_bytes(state)?;
        state.validate(&image, self.inner.cfg.max_memory_bytes)?;
        let native_hash = manifest.files[0].hash;
        let held = match base {
            Some(base) => Some(base.inner.native.clone()),
            None => self.inner.natives.get(&native_hash),
        };
        let native = match held {
            Some(held) => held,
            None => {
                let mut copy = native::NativeCopy::new()
                    .map_err(|error| PreparedError::Engine(error.into()))?;
                copy.write_all(native)
                    .map_err(|error| PreparedError::Engine(error.into()))?;
                // SAFETY: the bytes hash to what the product's manifest records, and a
                // product comes only from this engine's own capture or its trusted builder.
                let module = unsafe { copy.deserialize(&self.inner.wasmtime) }
                    .map_err(|error| PreparedError::Engine(error.into()))?;
                self.inner.natives.insert(native_hash, module)
            }
        };
        let mut template = self.finish_prepared(native, manifest, state, image)?;
        std::sync::Arc::get_mut(&mut template.inner)
            .ok_or(PreparedError::Invalid("new template is already shared"))?
            .prepared_hash = Some(manifest.module_hash);
        Ok(template)
    }

    /// Checks a local manifest and returns the exact content digest recorded by selection.
    pub fn prepared_manifest_sha256(&self, directory: &Path) -> Result<[u8; 32]> {
        let (manifest, digest) = files::read_manifest_receipt(directory)?;
        self.check_prepared(&manifest, manifest.identity)?;
        Ok(digest)
    }

    /// Maps the selected local artifact without compilation or
    /// initialization, as [`IsoplaneEngine::load_prepared`] does.
    ///
    /// # Safety
    /// As for [`IsoplaneEngine::load_prepared`].
    #[tracing::instrument(name = "template_load", skip_all)]
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn restore_prepared(
        &self,
        directory: &Path,
        manifest_sha256: [u8; 32],
        runtime_profile: [u8; 32],
        prefetch: &[ReadAhead],
    ) -> Result<Template> {
        let manifest = self.catalog_manifest(directory, manifest_sha256, runtime_profile)?;
        // SAFETY: the caller upholds this function's contract, which is the mapping's.
        unsafe { self.map_prepared(directory, manifest, prefetch, None) }
    }

    /// As [`IsoplaneEngine::restore_prepared`], for a template captured as a
    /// layer over `base`.
    ///
    /// # Safety
    /// As for [`IsoplaneEngine::load_prepared`].
    #[tracing::instrument(name = "template_load", skip_all)]
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn restore_prepared_over(
        &self,
        base: &Template,
        directory: &Path,
        manifest_sha256: [u8; 32],
        runtime_profile: [u8; 32],
        prefetch: &[ReadAhead],
    ) -> Result<Template> {
        let manifest = self.catalog_manifest(directory, manifest_sha256, runtime_profile)?;
        // SAFETY: the caller upholds this function's contract, which is the mapping's.
        unsafe { self.map_prepared(directory, manifest, prefetch, Some(base)) }
    }

    /// Maps a local compiled artifact matching the exact manifest digest
    /// recorded by local selection.
    /// Refuses a manifest whose digest, identity or engine
    /// compatibility differs from what is expected.
    ///
    /// # Safety
    /// As for [`IsoplaneEngine::load_prepared`].
    #[tracing::instrument(name = "template_load", skip_all)]
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn restore_prepared_by_digest(
        &self,
        directory: &Path,
        manifest_sha256: [u8; 32],
        expected: PreparedIdentity,
    ) -> Result<Template> {
        let manifest = files::read_manifest_by_digest(directory, manifest_sha256)?;
        self.check_prepared(&manifest, expected)?;
        // SAFETY: the caller upholds this function's contract, which is the mapping's.
        unsafe { self.map_prepared(directory, manifest, &[], None) }
    }

    /// Checks a selection receipt without reading native code or baseline payloads.
    pub fn check_prepared_receipt(
        &self,
        directory: &Path,
        manifest_sha256: [u8; 32],
        runtime_profile: [u8; 32],
    ) -> Result<PreparedIdentity> {
        Ok(self
            .catalog_manifest(directory, manifest_sha256, runtime_profile)?
            .identity)
    }

    fn catalog_manifest(
        &self,
        directory: &Path,
        manifest_sha256: [u8; 32],
        runtime_profile: [u8; 32],
    ) -> Result<Manifest> {
        let (manifest, digest) = files::read_manifest_receipt(directory)?;
        if digest != manifest_sha256 {
            return Err(PreparedError::Incompatible("prepared manifest digest"));
        }
        if manifest.identity.runtime_profile != runtime_profile {
            return Err(PreparedError::Incompatible("prepared runtime profile"));
        }
        self.check_prepared(&manifest, manifest.identity)?;
        Ok(manifest)
    }

    /// Checks a published local template and its payload hashes, then
    /// maps the same open baseline and state files. Native code is loaded from
    /// a private copy taken as it was verified; when a live template of this
    /// engine already holds the native payload the manifest records, it shares
    /// that module without reading this artifact's copy.
    ///
    /// On Linux, watched SIGBUS/BUS_ADRERR faults from lost whole baseline pages mark the
    /// template storage-failed; further activation is refused until it is loaded again.
    ///
    /// # Safety
    /// Keep state and readable baseline bytes unchanged for every returned template and
    /// instance; publication read-only permissions are not an isolation boundary.
    /// On Linux, loss of whole baseline backing pages is a supported failure only through
    /// watched reads, which mark the template storage-failed. Such data is invalid and must
    /// not be used for capture or further activation. Partial-page truncation and byte
    /// overwrites remain forbidden, as do native-code and state-file mutation.
    #[tracing::instrument(name = "template_load", skip_all)]
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn load_prepared(
        &self,
        directory: &Path,
        expected: PreparedIdentity,
        prefetch: &[ReadAhead],
    ) -> Result<Template> {
        let (manifest, _) = files::read_manifest_receipt(directory)?;
        self.check_prepared(&manifest, expected)?;
        // SAFETY: the caller upholds this function's contract, which is the mapping's.
        unsafe { self.map_prepared(directory, manifest, prefetch, None) }
    }

    /// As [`IsoplaneEngine::load_prepared`], for a template captured as a
    /// layer over `base`: its native code is `base`'s, and memory it left
    /// unchanged maps from `base`'s baseline file.
    ///
    /// # Safety
    /// As for [`IsoplaneEngine::load_prepared`], for this template's files and `base`'s.
    #[tracing::instrument(name = "template_load", skip_all)]
    /// Native bytes must originate from controlled local compilation and trusted operator-owned storage.
    /// Checksums do not establish compiler origin.
    pub unsafe fn load_prepared_over(
        &self,
        base: &Template,
        directory: &Path,
        expected: PreparedIdentity,
        prefetch: &[ReadAhead],
    ) -> Result<Template> {
        let (manifest, _) = files::read_manifest_receipt(directory)?;
        self.check_prepared(&manifest, expected)?;
        // SAFETY: the caller upholds this function's contract, which is the mapping's.
        unsafe { self.map_prepared(directory, manifest, prefetch, Some(base)) }
    }

    /// # Safety
    /// As for [`IsoplaneEngine::load_prepared`].
    unsafe fn map_prepared(
        &self,
        directory: &Path,
        manifest: Manifest,
        prefetch: &[ReadAhead],
        base: Option<&Template>,
    ) -> Result<Template> {
        let module_hash = manifest.module_hash;
        let base = matching_base(&manifest, base)?;
        let native_hash = manifest.files[0].hash;
        let held = match base {
            Some(base) => Some(base.inner.native.clone()),
            None => self.inner.natives.get(&native_hash),
        };
        let files = files::open_verified(directory, &manifest, held.is_none())?;
        files::advise(&files, &manifest, prefetch)?;
        let activation_prefetch = prefetch::Payloads::new(files.each(), directory)?;
        let files::Verified {
            native,
            baseline,
            state,
        } = files;
        #[cfg(target_os = "linux")]
        let layer_file = baseline
            .try_clone()
            .map_err(|error| PreparedError::Engine(error.into()))?;
        // SAFETY: the caller guarantees unchanged contents of this verified held inode and contains I/O faults.
        let baseline =
            unsafe { memmap2::Mmap::map(&baseline) }.map_err(|source| PreparedError::Io {
                operation: "map baseline",
                path: directory.join(BASELINE),
                source,
            })?;
        // SAFETY: this is the verified held inode; the caller guarantees unchanged contents and contains I/O faults.
        let state = unsafe { memmap2::Mmap::map(&state) }.map_err(|source| PreparedError::Io {
            operation: "map state",
            path: directory.join(STATE),
            source,
        })?;
        let state: state::State = postcard::from_bytes(&state)?;
        let baseline =
            crate::image::ImageStorage::mapped(baseline).map_err(|source| PreparedError::Io {
                operation: "watch baseline",
                path: directory.join(BASELINE),
                source,
            })?;
        let image = crate::MemoryImage::from_storage_over(
            baseline,
            base.map(|base| base.inner.image.clone()),
            self.inner.cfg.max_memory_bytes as usize,
        )
        .map_err(PreparedError::Invalid)?;
        state.validate(&image, self.inner.cfg.max_memory_bytes)?;
        let native = match (held, native) {
            (Some(held), _) => held,
            (None, Some(native)) => {
                let module = tracing::info_span!("template_native_deserialize")
                    .in_scope(|| {
                        // SAFETY: the caller guarantees local compiler origin; the private copy matches the manifest.
                        unsafe { native.deserialize(&self.inner.wasmtime) }
                    })
                    .map_err(|e| PreparedError::Engine(e.into()))?;
                self.inner.natives.insert(native_hash, module)
            }
            (None, None) => return Err(PreparedError::Invalid("native payload was not verified")),
        };
        #[cfg(target_os = "linux")]
        let layer = if image.is_layer() {
            let base_source = match base {
                Some(base) => Some(base.inner.layer_source.as_ref().ok_or(
                    PreparedError::Invalid("a base template must map its baseline"),
                )?),
                None => None,
            };
            Some(layer::from_file(layer_file, &image, base_source)?)
        } else {
            None
        };
        let activation_prefetch = activation_prefetch.select(&state.prefetch)?;
        activation_prefetch.warm()?;
        let template = self.finish_prepared(native, &manifest, state, image)?;
        #[cfg(target_os = "linux")]
        let template = {
            let mut template = template;
            if let Some(layer) = layer {
                layer::attach(&mut template, layer)?;
            }
            template
        };
        let template = {
            let mut template = template;
            let inner = std::sync::Arc::get_mut(&mut template.inner)
                .ok_or(PreparedError::Invalid("new template is already shared"))?;
            inner.activation_prefetch = Some(std::sync::Arc::new(activation_prefetch));
            inner.prepared_hash = Some(module_hash);
            template
        };
        Ok(template)
    }

    fn check_prepared(&self, manifest: &Manifest, expected: PreparedIdentity) -> Result<()> {
        if manifest.magic != MAGIC || manifest.format != FORMAT {
            return Err(PreparedError::Incompatible("schema"));
        }
        if manifest.abi_rev != isoplane_abi::ABI_REV {
            return Err(PreparedError::Incompatible("ABI"));
        }
        if manifest.engine != self.inner.fingerprint {
            return Err(PreparedError::Incompatible("engine/CPU profile"));
        }
        if manifest.identity != expected {
            return Err(PreparedError::Incompatible("input/runtime profile"));
        }
        Ok(())
    }
}

impl PreparedTemplateData {
    /// Fails unless every payload has the length and digest the manifest records.
    fn verify_payloads(&self) -> Result<()> {
        for (record, payload) in self.manifest.files.iter().zip(&self.payloads) {
            if record.bytes != payload.len() as u64
                || &record.hash != blake3::hash(payload).as_bytes()
            {
                return Err(PreparedError::Invalid("builder payload digest or length"));
            }
        }
        Ok(())
    }
}

impl PreparedTemplate {
    /// Exact engine compatibility identity attached by the producer.
    pub fn engine_fingerprint(&self) -> &str {
        &self.data.manifest.engine
    }

    /// Native compilation bytes for registration under this artifact's engine identity.
    pub fn native_bytes(&self) -> &[u8] {
        &self.data.payloads[PreparedPayload::Native.index()]
    }

    /// Encodes complete preparation output for the host.
    pub fn encode_builder_output(&self) -> Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }
}

/// `base` when `manifest` names it as the template it was captured over;
/// refuses a layer without its base and a base for anything but a layer.
fn matching_base<'a>(
    manifest: &Manifest,
    base: Option<&'a Template>,
) -> Result<Option<&'a Template>> {
    match (manifest.base, base) {
        (None, None) => Ok(None),
        (Some(expected), Some(base)) if base.inner.prepared_hash == Some(expected) => {
            if manifest.files[0].bytes != 0 {
                return Err(PreparedError::Invalid(
                    "a layer over a base carries no native code",
                ));
            }
            Ok(Some(base))
        }
        (Some(_), Some(_)) => Err(PreparedError::Incompatible("base template")),
        (Some(_), None) => Err(PreparedError::Incompatible(
            "a layer needs the base template it was captured over",
        )),
        (None, Some(_)) => Err(PreparedError::Incompatible(
            "only a layer is loaded over a base template",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> (IsoplaneEngine, PreparedTemplate, PreparedIdentity) {
        let engine = IsoplaneEngine::new(crate::EngineConfig {
            max_instances: 4,
            max_memory_bytes: 1 << 20,
            ..Default::default()
        })
        .expect("engine");
        let wasm = wat::parse_str("(module (memory (export \"memory\") 1) (global (export \"g\") (mut i32) (i32.const 9)) (data (i32.const 7) \"state\"))").expect("wasm");
        let identity = PreparedIdentity {
            input_hash: [1; 32],
            runtime_profile: [2; 32],
            initialization_hash: [3; 32],
        };
        let prepared = engine.prepare_template(&wasm, identity).expect("prepare");
        (engine, prepared, identity)
    }

    #[test]
    fn builder_output_retains_complete_prepared_payloads() {
        let (engine, prepared, identity) = prepared();
        let bytes = prepared.encode_builder_output().expect("encode");
        // SAFETY: this function created the output with this engine's trusted producer.
        let decoded = unsafe { engine.decode_builder_output(&bytes, identity) }.expect("decode");
        assert_eq!(
            decoded.data.payloads, prepared.data.payloads,
            "builder transport must retain native code and complete explicit state"
        );
    }

    #[test]
    fn builder_output_with_inconsistent_state_is_rejected() {
        let (engine, mut prepared, identity) = prepared();
        prepared.data.payloads[1][32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        prepared.data.manifest.files[1] = files::FileRecord::new(&prepared.data.payloads[1]);
        let bytes = prepared.encode_builder_output().expect("encode");
        // SAFETY: this is a deliberately malformed output from the test-owned producer, not customer code.
        assert!(
            matches!(
                unsafe { engine.decode_builder_output(&bytes, identity) },
                Err(PreparedError::Invalid(_))
            ),
            "a builder's invalid sparse index must fail before publication"
        );
    }
}
