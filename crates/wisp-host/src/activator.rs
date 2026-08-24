//! The real [`Activator`]: `wisp-engine`'s template and slot machinery,
//! wrapped in the seam the cell codes against (spec.md §4.4, §6.2).
//!
//! There is nothing to this file, and that is the point. §6.2's activation is
//! four steps — map the template memfd `MAP_PRIVATE`, initialize mutable
//! globals and tables from the template's sealed values, bind the import
//! vtable to this instance's tenant context, call `wisp-handle` — and all four
//! live in `wisp-engine` because all four touch wasmtime. The cell's interest
//! in an instance begins after that: which tier it is in, whose CPU it is
//! burning, and what happens to its memory next.
//!
//! The one thing worth stating here is why the template image is copied into
//! an `Arc<Vec<u8>>` at construction rather than borrowed per call: it is the
//! baseline for every hibernation page-diff (§6.6), taken on an execution-pool
//! thread while the engine's own copy stays mapped read-only and shared by
//! every instance of the module on the machine (§6.2). One copy per module per
//! cell, not one per instance.

use std::sync::Arc;

use wisp_engine::{HostBindings, Instance, InstanceOps, ModuleHash, Template, WispEngine};

use crate::instances::Activator;
use crate::ModuleId;

pub struct EngineActivator {
    engine: Arc<WispEngine>,
    template: Template,
    module: ModuleId,
    image: Arc<Vec<u8>>,
}

impl EngineActivator {
    pub fn new(engine: Arc<WispEngine>, template: Template, module: ModuleId) -> Self {
        let image = Arc::new(template.memory_image().to_vec());
        Self { engine, template, module, image }
    }

    /// Compile and pre-stage a template, then wrap it. §8.7 treats the module
    /// bytes as hostile input to the compiler, so this expects to be called
    /// with an artifact the validator sandbox has already produced.
    pub fn load(
        engine: Arc<WispEngine>,
        module: ModuleId,
        template_wasm: &[u8],
    ) -> crate::Result<Self> {
        let template = engine.load_template(template_wasm)?;
        Ok(Self::new(engine, template, module))
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
    fn template_image(&self) -> Arc<Vec<u8>> {
        self.image.clone()
    }
    fn activate(&self, bindings: Box<dyn HostBindings>) -> anyhow::Result<Box<dyn InstanceOps + Send>> {
        let inst: Instance = self.engine.activate(&self.template, bindings)?;
        Ok(Box::new(inst))
    }
}
