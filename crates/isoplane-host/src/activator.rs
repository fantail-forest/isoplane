//! The real [`Activator`]: `isoplane-engine`'s template and instance machinery
//! behind the seam the cell codes against. The template image is held as the
//! engine's own `Arc`, shared by every instance of the module, because it is
//! the baseline for every hibernation page diff.

use std::sync::Arc;

use isoplane_engine::{
    HostBindings, Instance, InstanceOps, IsoplaneEngine, MemoryImage, ModuleHash, Template,
};

use crate::pool::Activator;
use crate::ModuleId;

/// A reload produced a template other than the one it replaces, whose
/// hibernation deltas it could not restore.
#[derive(Debug, thiserror::Error)]
#[error("reloading module {module} produced a different template")]
pub struct ReloadedTemplateDiffers {
    pub module: ModuleId,
}

/// Loads a module's template again from its local prepared artifact.
pub type TemplateReload = Box<dyn Fn() -> crate::Result<Template> + Send + Sync>;

pub struct EngineActivator {
    engine: Arc<IsoplaneEngine>,
    template: parking_lot::RwLock<Arc<Template>>,
    module: ModuleId,
    reload: Option<TemplateReload>,
}

impl EngineActivator {
    pub fn new(engine: Arc<IsoplaneEngine>, template: Template, module: ModuleId) -> Self {
        Self {
            engine,
            template: parking_lot::RwLock::new(Arc::new(template)),
            module,
            reload: None,
        }
    }

    /// An activator that loads its template again through `reload` once a read
    /// of the template's mapped storage has failed, rather than refusing every
    /// later activation.
    pub fn reloading(
        engine: Arc<IsoplaneEngine>,
        template: Template,
        module: ModuleId,
        reload: TemplateReload,
    ) -> Self {
        Self {
            reload: Some(reload),
            ..Self::new(engine, template, module)
        }
    }

    /// Compiles and pre-stages a template. The module bytes are hostile input
    /// to the compiler, so this expects an artifact the validator sandbox has
    /// already produced.
    pub fn load(
        engine: Arc<IsoplaneEngine>,
        module: ModuleId,
        template_wasm: &[u8],
    ) -> crate::Result<Self> {
        let template = engine.load_template(template_wasm)?;
        Ok(Self::new(engine, template, module))
    }

    /// Loads by mapping a precompiled artifact out of `cache_dir`, compiling
    /// one into it only if it is not already there, and returns whether it had
    /// to compile. Unlike [`EngineActivator::load`], the code and memory image
    /// are page cache: demand-paged, evictable, and shared between every
    /// mapping of the same artifact.
    ///
    /// # Safety
    /// `cache_dir` must be a directory this node owns, holding only artifacts
    /// produced by an engine equal to `engine`. See [`IsoplaneEngine::load_mapped`].
    pub unsafe fn load_mapped(
        engine: Arc<IsoplaneEngine>,
        module: ModuleId,
        template_wasm: &[u8],
        cache_dir: &std::path::Path,
    ) -> crate::Result<(Self, bool)> {
        // SAFETY: delegated to this function's contract.
        let (template, compiled) = unsafe { engine.load_mapped(template_wasm, cache_dir) }?;
        Ok((Self::new(engine, template, module), compiled))
    }

    pub(crate) fn activate_raw(&self, bindings: Box<dyn HostBindings>) -> crate::Result<Instance> {
        self.engine.activate(&*self.current()?, bindings)
    }

    pub fn template(&self) -> Arc<Template> {
        self.template.read().clone()
    }

    /// The template to activate, reloaded first if its storage has failed.
    fn current(&self) -> crate::Result<Arc<Template>> {
        let template = self.template();
        let Some(reload) = self.reload.as_ref().filter(|_| template.storage_failed()) else {
            return Ok(template);
        };
        let mut current = self.template.write();
        if !current.storage_failed() {
            return Ok(current.clone());
        }
        let reloaded = Arc::new(reload()?);
        if reloaded.hash != current.hash {
            return Err(ReloadedTemplateDiffers {
                module: self.module.clone(),
            }
            .into());
        }
        tracing::info!(module = %self.module, "template reloaded after its storage failed");
        *current = reloaded.clone();
        Ok(reloaded)
    }
}

impl Activator for EngineActivator {
    fn map_activation(
        &self,
        memory_limit_bytes: isoplane_abi::config::GuestMemoryBytes,
    ) -> crate::Result<Option<Box<dyn isoplane_engine::DormantInstance>>> {
        self.engine
            .map_activation(&*self.current()?, memory_limit_bytes)
    }
    fn module(&self) -> &ModuleId {
        &self.module
    }
    fn module_hash(&self) -> ModuleHash {
        self.template.read().hash
    }
    fn imports(&self, module: &str, name: &str) -> bool {
        self.template.read().imports(module, name)
    }
    fn abi_rev(&self) -> u32 {
        self.template.read().abi_rev
    }
    fn template_image(&self) -> Arc<MemoryImage> {
        self.template.read().memory_image_arc()
    }
    fn warm_page_cache(&self) -> anyhow::Result<()> {
        self.template.read().warm_page_cache()
    }
    fn snapshot_size_bound(&self, max_memory_bytes: u64) -> anyhow::Result<u64> {
        isoplane_hibernate::local::size_bound(isoplane_hibernate::local::SnapshotShape {
            max_memory_bytes: max_memory_bytes.min(self.engine.configuration().max_memory_bytes),
            mutable_globals: self.template.read().mutable_global_count(),
        })
    }
    fn activate(
        &self,
        bindings: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let inst: Instance = self.engine.activate(&*self.current()?, bindings)?;
        Ok(Box::new(inst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_engine::EngineConfig;

    #[test]
    fn reservation_uses_the_engine_ceiling_and_the_templates_mutable_globals() {
        let engine = Arc::new(
            IsoplaneEngine::new(EngineConfig {
                max_instances: 2,
                max_memory_bytes: 2 << 20,
                guard_size_bytes: 64 << 20,
                ..EngineConfig::default()
            })
            .expect("engine must initialize"),
        );
        let module = wat::parse_str(
            r#"(module
            (memory (export "memory") 1)
            (global (export "counter") (mut i64) (i64.const 0))
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
            (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        )
        .expect("guest must parse");
        let activator = EngineActivator::load(engine, ModuleId::new("bound", "v1"), &module)
            .expect("guest must load");
        let expected =
            isoplane_hibernate::local::size_bound(isoplane_hibernate::local::SnapshotShape {
                max_memory_bytes: 2 << 20,
                mutable_globals: 1,
            })
            .expect("engine-limited snapshot must fit");
        assert_eq!(
            activator
                .snapshot_size_bound(4 << 20)
                .expect("reservation must fit"),
            expected,
            "reservation must include mutable globals and the engine's enforced memory ceiling"
        );
        let smaller = activator
            .snapshot_size_bound(1 << 20)
            .expect("smaller admission must fit");
        assert!(
            smaller < expected,
            "a smaller per-instance memory ceiling must reduce the reservation"
        );
    }

    #[cfg(target_os = "linux")]
    struct Unused;

    #[cfg(target_os = "linux")]
    impl HostBindings for Unused {
        fn begin_slice(&mut self) {}
        fn end_slice(&mut self) {}
        fn call_future(&mut self, _: isoplane_engine::ImportCall<'_>) -> u64 {
            panic!("a refused activation must not run the guest")
        }

        fn call_sync(&mut self, _: isoplane_engine::ImportCall<'_>) -> i64 {
            panic!("a refused activation must not run the guest")
        }

        fn slice_clock_ms(&self) -> u64 {
            0
        }
    }

    #[cfg(target_os = "linux")]
    fn guest(marker: i32) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
            (memory (export "memory") 1)
            (data (i32.const 64) "\{marker:02x}")
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
            (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#
        ))
        .expect("guest must parse")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_reload_that_yields_a_different_template_is_refused() {
        use isoplane_engine::prepared::PreparedIdentity;
        use std::os::unix::fs::PermissionsExt;
        let engine = Arc::new(
            IsoplaneEngine::new(EngineConfig {
                max_instances: 2,
                max_memory_bytes: 2 << 20,
                guard_size_bytes: 64 << 20,
                ..EngineConfig::default()
            })
            .expect("engine must initialize"),
        );
        let identity = PreparedIdentity {
            input_hash: [1; 32],
            runtime_profile: [2; 32],
            initialization_hash: [3; 32],
        };
        let directory = tempfile::tempdir().expect("artifact directory");
        let path = directory.path().join("version");
        engine
            .prepare_template(&guest(1), identity)
            .expect("prepare")
            .publish(&path)
            .expect("publish");
        // SAFETY: this engine compiled the artifact into this private directory.
        // Native code and state stay unchanged. Only whole baseline pages are
        // lost; the Linux read watch contains that fault and marks the template
        // failed before any guest execution below.
        let template =
            unsafe { engine.load_prepared(&path, identity, &[]) }.expect("map the template");
        let baseline = path.join("baseline.bin");
        std::fs::set_permissions(&baseline, std::fs::Permissions::from_mode(0o600))
            .expect("make the baseline writable");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&baseline)
            .and_then(|file| file.set_len(0))
            .expect("truncate the baseline");
        template
            .memory_image_arc()
            .page_eq(0, &[0; isoplane_engine::PAGE]);
        assert!(
            template.storage_failed(),
            "the fixture must fail the template's storage"
        );
        let other = engine.clone();
        let activator = EngineActivator::reloading(
            engine.clone(),
            template,
            ModuleId::new("reloaded", "v1"),
            Box::new(move || other.load_template(&guest(2))),
        );
        let refused = match activator.activate_raw(Box::new(Unused)) {
            Ok(_) => panic!("a reload must not swap in another module's template"),
            Err(error) => error,
        };
        assert!(
            refused.is::<ReloadedTemplateDiffers>(),
            "the refusal must name the mismatched reload: {refused:#}"
        );
    }
}
