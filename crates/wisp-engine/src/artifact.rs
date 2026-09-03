//! Template compilation and the on-disk artifact cache.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use wasmtime::Store;

use wisp_abi::imports::exports as guest_exports;

use crate::bindings::{HostBindings, ImportCall};
use crate::snapshot;
use crate::state::StoreState;
use crate::template::{self, ModuleHash, Template, TemplateInner};
use crate::{Result, WispEngine};

impl WispEngine {
    /// Cranelift compile. Parsing hostile input: the caller runs this inside
    /// the resource-capped validator sandbox.
    pub fn load_template(&self, template_wasm: &[u8]) -> Result<Template> {
        template::structural_check(template_wasm)?;
        let module = wasmtime::Module::new(&self.inner.wasmtime, template_wasm)
            .map_err(anyhow::Error::from)
            .context("compiling template module")?;
        let deferred = template::read_deferred_units(template_wasm)?;
        let mut_names = template::exported_mutable_globals(template_wasm)?;
        let table_names = template::exported_tables(template_wasm)?;
        self.finish_template(
            module,
            ModuleHash::of(template_wasm),
            deferred,
            mut_names,
            table_names,
        )
    }

    /// Compile a template's wasm to the two artifact parts, the metadata
    /// sidecar and the bare `.cwasm`, under this engine's configuration.
    /// Compiling with a differently configured engine produces a `.cwasm` the
    /// load path refuses. Two files because `Module::deserialize_file` maps a
    /// path and needs the `.cwasm` to start at byte 0.
    pub fn precompile(&self, template_wasm: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let t = self.load_template(template_wasm)?;
        let cwasm = t
            .inner
            .module
            .serialize()
            .map_err(anyhow::Error::from)
            .context("serializing compiled template")?;
        let meta = template::ArtifactMeta {
            magic: template::ARTIFACT_MAGIC,
            format: template::ARTIFACT_FORMAT,
            abi_rev: t.abi_rev,
            module_hash: t.hash.0,
            engine_fingerprint: self.inner.fingerprint.clone(),
            cwasm_hash: *blake3::hash(&cwasm).as_bytes(),
            memory_pages: t.inner.memory_pages,
            deferred: t.inner.deferred.clone(),
            mut_global_names: t.inner.mut_globals.iter().map(|(n, _)| n.clone()).collect(),
            table_names: t.inner.tables.iter().map(|(n, _)| n.clone()).collect(),
        };
        let meta = postcard::to_allocvec(&meta).context("encoding artifact metadata")?;
        Ok((meta, cwasm))
    }

    /// Load a template by mapping its compiled artifact, so the code and the
    /// memory image are page cache rather than a private copy per process.
    ///
    /// The `.cwasm` is checked against the hash in `meta` before anything is
    /// mapped, so a tampered or truncated file is refused with an error. What
    /// the check cannot cover is `meta` itself, hence the unsafety.
    ///
    /// # Safety
    /// `meta` must come from a source the caller trusts to have been written
    /// by [`WispEngine::precompile`]: a crafted metadata plus a matching
    /// `.cwasm` bypasses wasm validation and runs as native code. The file is
    /// mapped, not read, so nothing may rewrite it while the template is
    /// alive; a truncation under the mapping is a fault inside guest code, not
    /// an error return.
    pub unsafe fn deserialize_file(
        &self,
        meta: &[u8],
        cwasm_path: &std::path::Path,
    ) -> Result<Template> {
        let meta: template::ArtifactMeta =
            postcard::from_bytes(meta).context("decoding artifact metadata")?;
        self.check_artifact_meta(&meta)?;
        let cwasm = std::fs::read(cwasm_path)
            .with_context(|| format!("reading {}", cwasm_path.display()))?;
        if *blake3::hash(&cwasm).as_bytes() != meta.cwasm_hash {
            bail!(
                "{} does not match the hash in its metadata; the artifact was tampered with or \
                 truncated and will not be mapped",
                cwasm_path.display()
            );
        }
        drop(cwasm);
        // SAFETY: the file's contents were hashed against the metadata just
        // above, and the caller vouches for the metadata (see the doc comment).
        let module =
            unsafe { wasmtime::Module::deserialize_file(&self.inner.wasmtime, cwasm_path) }
                .map_err(anyhow::Error::from)
                .with_context(|| {
                    format!("mapping precompiled template {}", cwasm_path.display())
                })?;
        self.finish_template(
            module,
            ModuleHash(meta.module_hash),
            meta.deferred,
            meta.mut_global_names,
            meta.table_names,
        )
    }

    fn check_artifact_meta(&self, meta: &template::ArtifactMeta) -> Result<()> {
        if meta.magic != template::ARTIFACT_MAGIC {
            bail!("not a WISP template artifact");
        }
        if meta.format != template::ARTIFACT_FORMAT {
            bail!(
                "template artifact format {} is not {}",
                meta.format,
                template::ARTIFACT_FORMAT
            );
        }
        if meta.engine_fingerprint != self.inner.fingerprint {
            bail!(
                "template artifact was built by engine {} but this cell is {}; artifacts are \
                 keyed by module hash and engine fingerprint",
                meta.engine_fingerprint,
                self.inner.fingerprint
            );
        }
        if meta.abi_rev != wisp_abi::ABI_REV {
            bail!(
                "template artifact is ABI rev {} but this engine is rev {}",
                meta.abi_rev,
                wisp_abi::ABI_REV
            );
        }
        Ok(())
    }

    /// The artifact-cache key for a template on this engine, from the wasm
    /// alone so the cache can be consulted before paying for a compile.
    fn artifact_key(&self, template_wasm: &[u8]) -> String {
        format!(
            "{}-{}",
            ModuleHash::of(template_wasm).hex(),
            self.inner.fingerprint
        )
    }

    /// Compile a template's wasm to the two artifact files and drop the
    /// compiled module. Cranelift does not return its working set, so the
    /// intended caller is a short-lived process, after which a cell maps the
    /// result and never runs the compiler.
    pub fn precompile_to_files(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<ArtifactPaths> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key(template_wasm));
        let (meta, cwasm) = self.precompile(template_wasm)?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating artifact cache {}", dir.display()))?;
        write_atomic(&paths.meta, &meta)?;
        write_atomic(&paths.cwasm, &cwasm)?;
        Ok(paths)
    }

    /// Load a template by mapping its artifact, compiling one first only when
    /// the cache does not have it. Returns whether a compile happened.
    ///
    /// # Safety
    /// `dir` must be written only by this process through
    /// [`WispEngine::precompile_to_files`]. A cache another writer can reach
    /// can hand this cell arbitrary code; see [`WispEngine::deserialize_file`].
    pub unsafe fn load_mapped(
        &self,
        template_wasm: &[u8],
        dir: &std::path::Path,
    ) -> Result<(Template, bool)> {
        let paths = ArtifactPaths::new(dir, &self.artifact_key(template_wasm));
        let mut compiled = false;
        if !(paths.meta.exists() && paths.cwasm.exists()) {
            self.precompile_to_files(template_wasm, dir)?;
            compiled = true;
        }
        let meta = std::fs::read(&paths.meta)
            .with_context(|| format!("reading {}", paths.meta.display()))?;
        // SAFETY: the caller vouches for `dir`, which is where `meta` came from.
        let t = unsafe { self.deserialize_file(&meta, &paths.cwasm) }?;
        Ok((t, compiled))
    }

    fn finish_template(
        &self,
        module: wasmtime::Module,
        hash: ModuleHash,
        deferred: Vec<u32>,
        mut_names: Vec<String>,
        table_names: Vec<String>,
    ) -> Result<Template> {
        // Instantiate once to read back the pre-initialised image: the
        // baseline the page diff runs against. Activation maps wasmtime's own
        // CoW image and never copies from this.
        let called_import = Arc::new(AtomicBool::new(false));
        let mut probe_state = StoreState::new(
            Box::new(DenyAll {
                called: called_import.clone(),
            }),
            self.inner.cfg.max_memory_bytes as usize,
        );
        probe_state.clock_ms = 0;
        let mut store = Store::new(&self.inner.wasmtime, probe_state);
        store.limiter(|d| &mut d.limits);
        store.set_epoch_deadline(u64::MAX / 2);

        // The PAL adapter is composed per template: a trap stub is typed from
        // the module's own declaration, and every instance of the module
        // shares the result. A module importing only `wisp` takes the shared
        // linker.
        let needs_pal = module
            .imports()
            .any(|i| crate::pal::is_foreign_module(i.module()));
        let (pal_linker, pal_composed) = if needs_pal {
            let mut l = self.inner.linker.clone();
            let composed = crate::pal::link(&mut l, &module)
                .context("composing the WASI PAL adapter for a template")?;
            tracing::info!(
                composed = composed.len(),
                delegated = composed
                    .iter()
                    .filter(|d| d.disposition.is_delegated())
                    .count(),
                "composed the WASI PAL adapter for a template"
            );
            (Some(l), composed)
        } else {
            (None, Vec::new())
        };

        let inst = pal_linker
            .as_ref()
            .unwrap_or(&self.inner.linker)
            .instantiate(&mut store, &module)
            .map_err(anyhow::Error::from)
            .context("instantiating the template to read its pre-initialized image")?;
        if called_import.load(Ordering::Relaxed) {
            bail!(
                "template instantiation called a host import; a template whose start function \
                 reaches the host would bake one host response into every instance"
            );
        }

        let memory = inst
            .get_memory(&mut store, guest_exports::MEMORY)
            .with_context(|| format!("template does not export `{}`", guest_exports::MEMORY))?;
        let image = Arc::new(crate::image::MemoryImage::from_dense(memory.data(&store)));
        let memory_pages = memory.size(&store) as u32;

        let mut mut_globals = Vec::with_capacity(mut_names.len());
        for name in &mut_names {
            let g = inst
                .get_global(&mut store, name)
                .with_context(|| format!("exported mutable global `{name}` vanished"))?;
            let v = g.get(&mut store);
            mut_globals.push((name.clone(), snapshot::GlobalValue::of(v, &store)?));
        }

        let mut tables = Vec::with_capacity(table_names.len());
        let mut table_nullness = Vec::with_capacity(table_names.len());
        for name in &table_names {
            let t = inst
                .get_table(&mut store, name)
                .with_context(|| format!("exported table `{name}` vanished"))?;
            let size = t.size(&store);
            let mut nulls = Vec::with_capacity(size as usize);
            for i in 0..size {
                nulls.push(match t.get(&mut store, i) {
                    Some(wasmtime::Ref::Func(f)) => f.is_some(),
                    Some(wasmtime::Ref::Extern(e)) => e.is_some(),
                    Some(_) => true,
                    None => false,
                });
            }
            tables.push((name.clone(), size));
            table_nullness.push(nulls);
        }

        let has_gc_hint = inst.get_func(&mut store, guest_exports::GC_HINT).is_some();
        let has_initialize = inst
            .get_func(&mut store, guest_exports::INITIALIZE)
            .is_some();
        drop(store);

        Ok(Template {
            hash,
            abi_rev: wisp_abi::ABI_REV,
            inner: Arc::new(TemplateInner {
                module,
                pal_linker,
                pal_composed,
                image,
                memory_pages,
                deferred,
                mut_globals,
                tables,
                table_nullness,
                has_gc_hint,
                has_initialize,
            }),
        })
    }
}

/// Where one template's two artifact files live.
#[derive(Debug, Clone)]
pub struct ArtifactPaths {
    pub meta: std::path::PathBuf,
    pub cwasm: std::path::PathBuf,
}

impl ArtifactPaths {
    pub fn new(dir: &std::path::Path, key: &str) -> Self {
        ArtifactPaths {
            meta: dir.join(format!("{key}.meta")),
            cwasm: dir.join(format!("{key}.cwasm")),
        }
    }
}

/// Write to a temporary name and rename, so a reader that maps the file sees
/// the whole file or no file. A half-written `.cwasm` faults inside guest code
/// rather than failing to load.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Bindings for the probe instantiation in `finish_template`. Records that
/// an import was called so the template can be refused; reading the image
/// must have no side effects.
struct DenyAll {
    called: Arc<AtomicBool>,
}

impl HostBindings for DenyAll {
    fn call_future(&mut self, _call: ImportCall<'_>) -> u64 {
        self.called.store(true, Ordering::Relaxed);
        0
    }
    fn call_sync(&mut self, _call: ImportCall<'_>) -> i64 {
        self.called.store(true, Ordering::Relaxed);
        0
    }
    fn slice_clock_ms(&self) -> u64 {
        0
    }
}
