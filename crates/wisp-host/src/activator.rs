//! The real [`Activator`]: `wisp-engine`'s template and instance machinery
//! behind the seam the cell codes against. The template image is held as the
//! engine's own `Arc`, shared by every instance of the module, because it is
//! the baseline for every hibernation page diff.

use std::sync::Arc;

use wisp_engine::{
    HostBindings, Instance, InstanceOps, MemoryImage, ModuleHash, Template, WispEngine,
};

use crate::pool::Activator;
use crate::ModuleId;

pub struct EngineActivator {
    engine: Arc<WispEngine>,
    template: Template,
    module: ModuleId,
    image: Arc<MemoryImage>,
}

impl EngineActivator {
    pub fn new(engine: Arc<WispEngine>, template: Template, module: ModuleId) -> Self {
        let image = template.memory_image_arc();
        Self {
            engine,
            template,
            module,
            image,
        }
    }

    /// Compiles and pre-stages a template. The module bytes are hostile input
    /// to the compiler, so this expects an artifact the validator sandbox has
    /// already produced.
    pub fn load(
        engine: Arc<WispEngine>,
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
    /// produced by an engine equal to `engine`. See [`WispEngine::load_mapped`].
    pub unsafe fn load_mapped(
        engine: Arc<WispEngine>,
        module: ModuleId,
        template_wasm: &[u8],
        cache_dir: &std::path::Path,
    ) -> crate::Result<(Self, bool)> {
        // SAFETY: delegated to this function's contract.
        let (template, compiled) = unsafe { engine.load_mapped(template_wasm, cache_dir) }?;
        Ok((Self::new(engine, template, module), compiled))
    }

    pub(crate) fn activate_raw(&self, bindings: Box<dyn HostBindings>) -> crate::Result<Instance> {
        self.engine.activate(&self.template, bindings)
    }

    pub fn template(&self) -> &Template {
        &self.template
    }
}

impl Activator for EngineActivator {
    fn module(&self) -> &ModuleId {
        &self.module
    }
    fn module_hash(&self) -> ModuleHash {
        self.template.hash
    }
    fn imports(&self, module: &str, name: &str) -> bool {
        self.template.imports(module, name)
    }
    fn abi_rev(&self) -> u32 {
        self.template.abi_rev
    }
    fn template_image(&self) -> Arc<MemoryImage> {
        self.image.clone()
    }
    fn snapshot_size_bound(&self, max_memory_bytes: u64) -> anyhow::Result<u64> {
        Ok(crate::hibernate_capture::snapshot_size_bound(
            crate::hibernate_capture::SnapshotShape {
                max_memory_bytes: max_memory_bytes
                    .min(self.engine.configuration().max_memory_bytes),
                mutable_globals: self.template.mutable_global_count(),
            },
        )?)
    }
    fn activate(
        &self,
        bindings: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let inst: Instance = self.engine.activate(&self.template, bindings)?;
        Ok(Box::new(inst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_engine::EngineConfig;

    #[test]
    fn reservation_uses_the_engine_ceiling_and_the_templates_mutable_globals() {
        let engine = Arc::new(
            WispEngine::new(EngineConfig {
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
            (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
            (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
            (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 0)))"#,
        )
        .expect("guest must parse");
        let activator = EngineActivator::load(engine, ModuleId::new("bound", "v1"), &module)
            .expect("guest must load");
        let expected = crate::hibernate_capture::snapshot_size_bound(
            crate::hibernate_capture::SnapshotShape {
                max_memory_bytes: 2 << 20,
                mutable_globals: 1,
            },
        )
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
}
