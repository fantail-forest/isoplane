//! A runtime baseline, prepared once per runtime, engine and limits into the
//! user's runtime cache, and reused by every preparation of an application
//! package on it, whichever project the package comes from. The application
//! is prepared as a layer over it into the project's own cache, and reused
//! while its identity holds.

use super::Project;

/// The runtime baseline a session's package preparations reuse. Clones share it.
#[derive(Clone, Default)]
pub struct Baselines {
    current: std::sync::Arc<parking_lot::Mutex<Option<isoplane_prepare::RuntimeBaseline>>>,
}

impl Baselines {
    /// Prepares `project`'s application package as a layer over the runtime
    /// `wasm`. The baseline is the one the session holds for the same
    /// runtime, engine and limits, else the one published in the runtime
    /// cache, else prepared and published now. `admit` runs only in that
    /// last case, since any other baseline came from the same admitted
    /// module. `None` for a module that carries its application.
    pub(super) fn prepare(
        &self,
        project: &Project,
        wasm: &[u8],
        settings: &[u8],
        version: &str,
        admit: impl FnOnce() -> anyhow::Result<()>,
    ) -> Option<anyhow::Result<super::Prepared>> {
        if project.application_files.files().is_empty() {
            return None;
        }
        Some(self.prepare_package(project, wasm, settings, version, admit))
    }

    fn prepare_package(
        &self,
        project: &Project,
        wasm: &[u8],
        settings: &[u8],
        version: &str,
        admit: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<super::Prepared> {
        let request = request(project, wasm, settings, version)?;
        let wanted = request.baseline();
        let identity = wanted.identity()?;
        let mut current = self.current.lock();
        let (baseline, compiled) = match current.as_ref() {
            Some(held) if held.identity() == identity => (held.clone(), false),
            _ => {
                let (baseline, compiled) = published(project, wasm, &wanted, admit)?;
                *current = Some(baseline.clone());
                (baseline, compiled)
            }
        };
        drop(current);
        let (mut template, reused) = application(project, &baseline, &request)?;
        template.retain_storage(std::sync::Arc::new(baseline.clone()));
        tracing::info!(
            compiled,
            reused,
            "local preparation over the runtime baseline ready"
        );
        let module = isoplane_host::ModuleId::new(&project.manifest.name, version);
        let activator = std::sync::Arc::new(isoplane_host::EngineActivator::new(
            std::sync::Arc::clone(baseline.engine()),
            template,
            module,
        ));
        Ok(super::Prepared {
            activator: super::PreparedActivator::Ordinary(activator),
            manifest: project.manifest.clone(),
            tenant: project.tenant(),
            version: version.to_string(),
        })
    }
}

/// Publishes the runtime baseline for `wasm` in the runtime cache, as the
/// first preparation of a package on it would, and returns whether that
/// compiled the runtime. `admit` runs only when it does.
pub(super) fn prepare_runtime(
    project: &Project,
    wasm: &[u8],
    admit: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    // A baseline's identity leaves out the application's settings and version.
    let request = request(project, wasm, b"{}", "")?;
    let (_, compiled) = published(project, wasm, &request.baseline(), admit)?;
    Ok(compiled)
}

/// The project's application, loaded over `baseline` from the layer a
/// previous preparation published for the same identity, or prepared now and
/// published for the next one. Returns whether it was reused. Every other
/// layer of the project is removed: one with another identity holds an
/// application, or settings, the project no longer has.
fn application(
    project: &Project,
    baseline: &isoplane_prepare::RuntimeBaseline,
    request: &isoplane_prepare::BuildRequest,
) -> anyhow::Result<(isoplane_engine::Template, bool)> {
    let layers = application_layers(project)?;
    let identity = request.identity()?;
    let name = identity_name(&identity);
    let directory = layers.directory().join(&name);
    let engine = baseline.engine();
    // SAFETY: the directory lies in the project's private cache, under a lease this
    // preparation holds. Only `publish_application` writes locally compiled native output there, publishing each layer
    // atomically as files that nothing rewrites while a template maps them.
    let loaded =
        unsafe { engine.load_prepared_over(baseline.template(), &directory, identity, &[]) };
    let (mut template, reused) = match loaded {
        Ok(template) => {
            tracing::info!(path = %directory.display(), "reusing the prepared application");
            (template, true)
        }
        Err(error) => {
            if directory.exists() {
                tracing::warn!(path = %directory.display(), %error, "preparing the application again");
                std::fs::remove_dir_all(&directory)?;
            }
            let application = baseline
                .prepare_application_in(request, layers.directory())
                .map_err(isoplane_prepare::PrepareError::Rejected)?;
            publish_application(
                engine,
                baseline.template(),
                &application,
                identity,
                &directory,
            );
            (application.template, false)
        }
    };
    remove_other_layers(layers.directory(), &name);
    template.retain_storage(layers);
    Ok((template, reused))
}

/// Publishes a freshly prepared layer for the next preparation. A failure
/// costs only that reuse, so it is reported and the session goes on.
fn publish_application(
    engine: &isoplane_engine::IsoplaneEngine,
    base: &isoplane_engine::Template,
    application: &isoplane_prepare::PreparedApplication,
    identity: isoplane_engine::prepared::PreparedIdentity,
    directory: &std::path::Path,
) {
    // SAFETY: `prepare_application_in` produced these bytes in this process.
    let published = unsafe {
        engine.decode_builder_output_over(base, application.product.prepared_bytes(), identity)
    }
    .and_then(|product| product.publish(directory));
    match published {
        Ok(_) => {}
        // Another session of the project published the same layer first.
        Err(_) if directory.exists() => {}
        Err(error) => {
            tracing::warn!(path = %directory.display(), %error, "the prepared application is not kept for the next session");
        }
    }
}

/// Removes the layers published for other identities. One still mapped by a
/// running session cannot be removed on Windows; the next preparation tries again.
fn remove_other_layers(layers: &std::path::Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(layers) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == current || !is_identity_name(name) {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(entry.path()) {
            tracing::warn!(path = %entry.path().display(), %error, "an earlier prepared application is not removed yet");
        }
    }
}

/// The project's cache entry for its prepared application layers. Its key
/// does not depend on the runtime or engine, so the layers every version
/// published stay in one place to be checked; a layer that no longer matches
/// is refused when it loads.
fn application_layers(project: &Project) -> anyhow::Result<std::sync::Arc<isoplane_cache::Lease>> {
    let storage = isoplane_cache::Cache::new(
        project.compilation_directory.clone(),
        isoplane_cache::PROJECT_CACHE_BYTES,
    )?;
    let key = blake3::derive_key("isoplane local application layers v1", &[]);
    Ok(std::sync::Arc::new(
        storage.lease(isoplane_cache::CacheKey(key))?,
    ))
}

fn identity_name(identity: &isoplane_engine::prepared::PreparedIdentity) -> String {
    let mut name = blake3::Hasher::new();
    for part in [
        &identity.input_hash,
        &identity.runtime_profile,
        &identity.initialization_hash,
    ] {
        name.update(part);
    }
    name.finalize().to_hex().to_string()
}

fn is_identity_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn request(
    project: &Project,
    wasm: &[u8],
    settings: &[u8],
    version: &str,
) -> anyhow::Result<isoplane_prepare::BuildRequest> {
    let manifest = &project.manifest;
    Ok(isoplane_prepare::BuildRequest {
        engine: super::engine_configuration(project),
        application_files: project.application_files.clone(),
        runtime: project.runtime_profile.clone(),
        plan: manifest.limits,
        guest_memory_bytes: manifest.guest_memory_bytes,
        validation_limits: manifest.validation_limits,
        initialization: isoplane_prepare::Initialization {
            network: Some(super::initialization_network(project)),
            config: serde_json::to_vec(&manifest.init_visible_vars)?,
            runtime_config: settings.to_vec(),
            sandbox: project.init_sandbox,
            occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        },
        input_hash: *blake3::hash(wasm).as_bytes(),
        portable_template: isoplane_prepare::PortableTemplate::Omitted,
    })
}

/// Where an entry keeps the compiler's own copy of the runtime while its
/// baseline is prepared. A published baseline carries the same code, so the
/// copy is removed then; until then it lets a preparation that was
/// interrupted resume without compiling again.
const COMPILATION: &str = "compilation";

/// The published baseline for `wanted`, loaded from the runtime cache, or
/// prepared from `wasm` after `admit` and published there first. Returns
/// whether preparing it compiled the runtime.
fn published(
    project: &Project,
    wasm: &[u8],
    wanted: &isoplane_prepare::BaselineRequest,
    admit: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(isoplane_prepare::RuntimeBaseline, bool)> {
    use isoplane_prepare::{PrepareError, RuntimeBaseline};
    let identity = wanted.identity()?;
    let mut name = blake3::Hasher::new();
    for part in [
        &identity.input_hash,
        &identity.runtime_profile,
        &identity.initialization_hash,
    ] {
        name.update(part);
    }
    let cache = super::runtime_cache(project, name.finalize().as_bytes())?;
    let directory = cache.lease.directory().join("baseline");
    let engine = std::sync::Arc::new(isoplane_engine::IsoplaneEngine::new(
        super::engine_configuration(project),
    )?);
    match load(&engine, &directory, identity) {
        Ok(mut template) => {
            template.retain_storage(cache.lease.clone());
            tracing::info!(path = %directory.display(), "reusing the local runtime baseline");
            return Ok((RuntimeBaseline::loaded(engine, template, wanted)?, false));
        }
        Err(error) if directory.exists() => {
            tracing::warn!(path = %directory.display(), %error, "preparing the local runtime baseline again");
            std::fs::remove_dir_all(&directory)?;
        }
        Err(_) => {}
    }
    admit()?;
    let compilation = cache.lease.directory().join(COMPILATION);
    // SAFETY: only the compiler writes into this private, leased directory.
    let compiler =
        unsafe { isoplane_engine::CompilationCache::from_trusted_directory(compilation.clone()) }?;
    let prepared =
        RuntimeBaseline::prepare(wasm, wanted, Some(&compiler)).map_err(PrepareError::Rejected)?;
    let engine = std::sync::Arc::clone(prepared.baseline.engine());
    let compiled = prepared.product.initialized.compiled;
    // SAFETY: RuntimeBaseline::prepare produced these bytes in this process.
    let product =
        unsafe { engine.decode_builder_output(prepared.product.prepared_bytes(), identity) }?;
    drop(prepared);
    if let Err(error) = product.publish(&directory) {
        // Another session preparing the same baseline published it first.
        if !directory.exists() {
            return Err(error.into());
        }
    }
    let mut template = load(&engine, &directory, identity)?;
    template.retain_storage(cache.lease.clone());
    if let Err(error) = std::fs::remove_dir_all(&compilation) {
        tracing::warn!(path = %compilation.display(), %error, "the runtime's compilation scratch is not removed yet");
    }
    Ok((RuntimeBaseline::loaded(engine, template, wanted)?, compiled))
}

fn load(
    engine: &isoplane_engine::IsoplaneEngine,
    directory: &std::path::Path,
    identity: isoplane_engine::prepared::PreparedIdentity,
) -> Result<isoplane_engine::Template, isoplane_engine::prepared::PreparedError> {
    // SAFETY: the directory lies in the user's runtime cache, which
    // `runtime_cache` checked is private. Only `published` writes baselines
    // there, publishing each atomically as files that nothing rewrites while
    // a template maps them.
    unsafe { engine.load_prepared(directory, identity, &[]) }
}
