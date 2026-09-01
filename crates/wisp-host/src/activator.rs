//! The real [`Activator`]: `wisp-engine`'s template and slot machinery,
//! wrapped in the seam the cell codes against.
//!
//! There is nothing to this file, and that is the point. Activation is four
//! steps — map the template memfd `MAP_PRIVATE`, initialize mutable
//! globals and tables from the template's sealed values, bind the import
//! vtable to this instance's tenant context, call `wisp-handle` — and all four
//! live in `wisp-engine` because all four touch wasmtime. The cell's interest
//! in an instance begins after that: which tier it is in, whose CPU it is
//! burning, and what happens to its memory next.
//!
//! The one thing worth stating here is why the template image is held as an
//! `Arc` at construction rather than borrowed per call: it is the baseline for
//! every hibernation page-diff, taken on an execution-pool thread while
//! the engine's own copy stays mapped read-only and shared by every instance
//! of the module on the machine.
//!
//! It used to be `Arc::new(template.memory_image().to_vec())` — a **second**
//! full copy of the image per module per cell, for a baseline the engine was
//! already holding. It is now the engine's own `Arc`, shared.

use std::sync::Arc;

use wisp_engine::{HostBindings, Instance, InstanceOps, MemoryImage, ModuleHash, Template, WispEngine};

use crate::instances::Activator;
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
        Self { engine, template, module, image }
    }

    /// Compile and pre-stage a template, then wrap it. The module bytes are
    /// hostile input to the compiler, so this expects to be called with an
    /// artifact the validator sandbox has already produced.
    pub fn load(
        engine: Arc<WispEngine>,
        module: ModuleId,
        template_wasm: &[u8],
    ) -> crate::Result<Self> {
        let template = engine.load_template(template_wasm)?;
        Ok(Self::new(engine, template, module))
    }

    /// Load by **mapping** a precompiled artifact out of `cache_dir`,
    /// compiling one into it only if it is not already there.
    ///
    /// This is what a cell should call. [`EngineActivator::load`] copies the
    /// compiled artifact into anonymous memory and makes all of it resident;
    /// this maps it, so the code and the memory image are page cache —
    /// demand-paged, evictable, and shared between every mapping of the same
    /// artifact, and the template's memfd shmem stops existing. The gap is
    /// widest for a large pre-initialized template.
    ///
    /// Returns whether it had to compile. A cell that reports `true` on every
    /// load has a cache that is not working, which is worth being able to see:
    /// the compile is the expensive half, and it leaves the compiled artifact
    /// resident in this process for good.
    ///
    /// # Safety
    /// `cache_dir` must be a directory this node owns, holding only artifacts
    /// produced by an engine equal to `engine`. See [`WispEngine::load_mapped`].
    #[allow(unsafe_code)] // wasmtime's `deserialize_file` contract; see above.
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
    fn abi_rev(&self) -> u32 {
        self.template.abi_rev
    }
    fn template_image(&self) -> Arc<MemoryImage> {
        self.image.clone()
    }
    fn activate(&self, bindings: Box<dyn HostBindings>) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let inst: Instance = self.engine.activate(&self.template, bindings)?;
        Ok(Box::new(inst))
    }
}
