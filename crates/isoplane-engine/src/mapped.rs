//! Non-callable activations whose mappings outlive their resident pages.

use anyhow::{ensure, Context};

use crate::bindings::Bindings;
use crate::state::StoreState;
use crate::{HostBindings, Instance, InstanceOps, IsoplaneEngine, Result, Template};

/// A retained address space. Activation restores bindings; a hibernated instance
/// also requires its saved delta before guest entry.
pub trait DormantInstance: Send {
    /// Rebind this address space without reinstantiating or replacing its mappings.
    fn activate(
        self: Box<Self>,
        bindings: Box<dyn HostBindings>,
    ) -> Result<Box<dyn InstanceOps + Send>>;
}

struct Mapped(Instance);

impl IsoplaneEngine {
    /// Map a captured application before dispatch admission, without host resources.
    /// Returns `None` without Linux pooling or a captured template image.
    /// The retained activation consumes one slot from the engine's capacity.
    pub fn map_activation(
        &self,
        template: &Template,
        memory_limit_bytes: isoplane_abi::config::GuestMemoryBytes,
    ) -> Result<Option<Box<dyn DormantInstance>>> {
        if !self.inner.cfg.pooling || !supported(template) {
            return Ok(None);
        }
        let live = crate::activate::instantiate(
            &self.inner,
            &template.inner,
            StoreState::unbound(&self.inner.cfg, memory_limit_bytes.get()),
        )?;
        #[cfg(target_os = "linux")]
        let mut live = live;
        #[cfg(target_os = "linux")]
        {
            let ceiling_bytes = live.store.data().limits.max_memory_bytes;
            live.memory
                .prepare_growth(&mut live.store, ceiling_bytes)
                .map_err(anyhow::Error::from)
                .context("preparing guest memory growth before admission")?;
        }
        let mut instance = Instance {
            engine: self.inner.clone(),
            template: template.inner.clone(),
            live: Some(live),
            discarded: None,
            last: None,
            last_violation: None,
        };
        discard_pages(&mut instance)?;
        Ok(Some(Box::new(Mapped(instance))))
    }
}

fn supported(template: &Template) -> bool {
    #[cfg(target_os = "linux")]
    {
        template.inner.initialization_layer.is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = template;
        false
    }
}

pub(crate) fn evict(instance: Instance) -> Option<Box<dyn DormantInstance>> {
    #[cfg(target_os = "linux")]
    let mut instance = instance;
    #[cfg(target_os = "linux")]
    if instance.engine.cfg.pooling
        && instance.template.initialization_layer.is_some()
        && instance.is_capture_eligible()
    {
        match discard_pages(&mut instance) {
            Ok(()) => return Some(Box::new(Mapped(instance))),
            Err(error) => {
                tracing::warn!(%error, "page eviction failed; releasing saved instance mappings")
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    drop(instance);
    None
}

fn discard_pages(instance: &mut Instance) -> Result<()> {
    let live = instance
        .live
        .as_mut()
        .context("instance has no live store")?;
    discard_linear_pages(live)?;
    live.store.data_mut().bindings = None;
    live.store.data_mut().scratch = Vec::new();
    Ok(())
}

fn discard_linear_pages(live: &mut crate::instance::Live) -> Result<()> {
    ensure!(
        live.store.data().wasm_depth == 0,
        "cannot evict an executing guest"
    );
    #[cfg(target_os = "linux")]
    {
        let length = live.memory.data_size(&live.store);
        if length != 0 {
            // SAFETY: exclusive ownership of the idle store excludes guest entry.
            // The full page-aligned range is owned by its private template mapping;
            // callers either saved mutable state or deliberately reset to the template.
            let result = unsafe {
                libc::madvise(
                    live.memory.data_ptr(&live.store).cast(),
                    length,
                    libc::MADV_DONTNEED,
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error()).context("discarding guest pages");
            }
        }
    }
    Ok(())
}

impl DormantInstance for Mapped {
    fn activate(
        mut self: Box<Self>,
        bindings: Box<dyn HostBindings>,
    ) -> Result<Box<dyn InstanceOps + Send>> {
        let instance = &mut self.0;
        ensure!(
            !instance.template.storage_failed(),
            "retained template storage failed"
        );
        if let Some(payloads) = &instance.template.activation_prefetch {
            payloads.read_all()?;
        }
        let mut bindings = Bindings::Runtime(bindings);
        bindings.restore_prepared_resources(&instance.template.prepared_resources)?;
        let live = instance.live.as_mut().context("retained store missing")?;
        let mut data = StoreState::with_bindings(bindings, &instance.engine.cfg);
        data.limits.max_memory_bytes = data
            .limits
            .max_memory_bytes
            .min(live.store.data().limits.max_memory_bytes);
        ensure!(
            live.memory.data_size(&live.store) <= data.limits.max_memory_bytes,
            "retained memory exceeds instance limit"
        );
        data.memory = Some(live.memory);
        data.platform_files = instance.template.filesystem_files.clone();
        *live.store.data_mut() = data;
        crate::activate::restore_globals(&mut live.store, &live.globals, &instance.template)?;
        instance.last = None;
        instance.last_violation = None;
        Ok(Box::new(self.0))
    }
}

pub(crate) fn reset(instance: &mut Instance) -> Result<bool> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = instance;
        Ok(false)
    }
    #[cfg(target_os = "linux")]
    {
        if !instance.engine.cfg.pooling || instance.template.initialization_layer.is_none() {
            return Ok(false);
        }
        if instance.template.storage_failed() {
            return Err(crate::TemplateStorageFailed.into());
        }
        let live = instance
            .live
            .as_mut()
            .context("instance has no live store")?;
        if live.memory.size(&live.store) != u64::from(instance.template.memory_pages) {
            return Ok(false);
        }
        match live
            .table_baseline
            .verify(&mut live.store, &live.tables, &instance.template)
        {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::snapshot::SnapshotError>(),
                    Some(crate::snapshot::SnapshotError::TableMutation { .. })
                ) =>
            {
                return Ok(false)
            }
            Err(error) => return Err(error),
        }
        if let Some(bindings) = live.store.data_mut().bindings.as_mut() {
            bindings.reset_prepared_resources(&instance.template.prepared_resources)?;
        }
        discard_linear_pages(live)?;
        live.store.data_mut().reset_execution();
        live.store.data_mut().memory = Some(live.memory);
        crate::activate::restore_globals(&mut live.store, &live.globals, &instance.template)?;
        Ok(true)
    }
}
