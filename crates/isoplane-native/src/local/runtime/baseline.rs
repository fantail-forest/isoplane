//! A runtime baseline, prepared once per runtime, engine and limits into the
//! project's compiler-owned storage, and reused by every preparation of an
//! application package on it.

use super::Project;

/// The runtime baseline a session's package preparations reuse. Clones share it.
#[derive(Clone, Default)]
pub struct Baselines {
    #[cfg(target_os = "linux")]
    current: std::sync::Arc<parking_lot::Mutex<Option<isoplane_prepare::RuntimeBaseline>>>,
}

impl Baselines {
    /// Prepares `project`'s application package as a layer over the runtime
    /// `wasm`. The baseline is the one the session holds for the same
    /// runtime, engine and limits, else the one published in compiler-owned
    /// storage, else prepared and published now. `admit` runs only in that
    /// last case, since any other baseline came from the same admitted
    /// module. `None` for a module that carries its application, and on
    /// every platform but Linux, where templates are not mapped from layers.
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
        #[cfg(target_os = "linux")]
        return Some(self.prepare_package(project, wasm, settings, version, admit));
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (wasm, settings, version, admit);
            None
        }
    }

    #[cfg(target_os = "linux")]
    fn prepare_package(
        &self,
        project: &Project,
        wasm: &[u8],
        settings: &[u8],
        version: &str,
        admit: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<super::Prepared> {
        let request = request(project, wasm, settings)?;
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
        let application = baseline
            .prepare_application(&request)
            .map_err(isoplane_prepare::PrepareError::Rejected)?;
        tracing::info!(
            compiled,
            "local preparation over the runtime baseline ready"
        );
        let module = isoplane_host::ModuleId::new(&project.manifest.name, version);
        let activator = std::sync::Arc::new(isoplane_host::EngineActivator::new(
            std::sync::Arc::clone(baseline.engine()),
            application.template,
            module,
        ));
        Ok(super::Prepared {
            activator: super::PreparedActivator::Ordinary(activator),
            manifest: project.manifest.clone(),
            tenant: project.tenant(),
            version: version.to_string(),
            allow_endpoints: project.allow_endpoints.clone(),
            compiled,
        })
    }
}

#[cfg(target_os = "linux")]
fn request(
    project: &Project,
    wasm: &[u8],
    settings: &[u8],
) -> anyhow::Result<isoplane_prepare::BuildRequest> {
    let manifest = &project.manifest;
    Ok(isoplane_prepare::BuildRequest {
        engine: super::engine_configuration(project),
        application_files: project.application_files.clone(),
        runtime: isoplane_prepare::RuntimeProfile::raw_wasm(),
        plan: manifest.limits,
        guest_memory_bytes: manifest.guest_memory_bytes,
        validation_limits: manifest.validation_limits,
        initialization: isoplane_prepare::Initialization {
            network: Some(super::initialization_network(project)),
            config: serde_json::to_vec(&manifest.init_visible_vars)?,
            runtime_config: settings.to_vec(),
            sandbox: project.init_sandbox,
            cpu_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_CPU_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        },
        input_hash: *blake3::hash(wasm).as_bytes(),
        portable_template: isoplane_prepare::PortableTemplate::Omitted,
    })
}

/// The published baseline for `wanted`, loaded from the project's
/// compiler-owned storage, or prepared from `wasm` after `admit` and
/// published there first. Returns whether preparing it compiled the runtime.
#[cfg(target_os = "linux")]
fn published(
    project: &Project,
    wasm: &[u8],
    wanted: &isoplane_prepare::BaselineRequest,
    admit: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(isoplane_prepare::RuntimeBaseline, bool)> {
    use isoplane_prepare::{PrepareError, RuntimeBaseline};
    let identity = wanted.identity()?;
    let cache = super::compilation_cache(project)?;
    let mut name = blake3::Hasher::new();
    for part in [
        &identity.input_hash,
        &identity.runtime_profile,
        &identity.initialization_hash,
    ] {
        name.update(part);
    }
    let directory = super::runtime_directory(project)?
        .join("baselines")
        .join(name.finalize().to_hex().as_str());
    let engine = std::sync::Arc::new(isoplane_engine::IsoplaneEngine::new(
        super::engine_configuration(project),
    )?);
    match load(&engine, &directory, identity) {
        Ok(template) => {
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
    let prepared =
        RuntimeBaseline::prepare(wasm, wanted, Some(&cache)).map_err(PrepareError::Rejected)?;
    let engine = std::sync::Arc::clone(prepared.baseline.engine());
    // SAFETY: RuntimeBaseline::prepare produced these bytes in this process.
    let product =
        unsafe { engine.decode_builder_output(prepared.product.prepared_bytes(), identity) }?;
    if let Err(error) = product.publish(&directory) {
        // Another session preparing the same baseline published it first.
        if !directory.exists() {
            return Err(error.into());
        }
    }
    let template = load(&engine, &directory, identity)?;
    Ok((
        RuntimeBaseline::loaded(engine, template, wanted)?,
        prepared.product.initialized.compiled,
    ))
}

#[cfg(target_os = "linux")]
fn load(
    engine: &isoplane_engine::IsoplaneEngine,
    directory: &std::path::Path,
    identity: isoplane_engine::prepared::PreparedIdentity,
) -> Result<isoplane_engine::Template, isoplane_engine::prepared::PreparedError> {
    // SAFETY: the directory lies in the project's compiler-owned storage, which
    // `compilation_cache` checked is private. Only `published` writes locally compiled baselines
    // there, publishing each atomically as read-only files that nothing
    // rewrites while a template maps them.
    unsafe { engine.load_prepared(directory, identity, &[]) }
}
