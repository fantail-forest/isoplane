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
    fn activate(
        &self,
        bindings: Box<dyn HostBindings>,
    ) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let inst: Instance = self.engine.activate(&self.template, bindings)?;
        Ok(Box::new(inst))
    }
}
