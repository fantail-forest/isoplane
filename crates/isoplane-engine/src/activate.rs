//! Instance activation: mapping a template copy-on-write and wiring the
//! epoch and call hooks that make a dispatch meterable.

use std::sync::Arc;

use anyhow::Context;
use wasmtime::{CallHook, Extern, Store, UpdateDeadline};

use isoplane_abi::imports::exports as guest_exports;

use crate::bindings::{Bindings, HostBindings, InitializationBindings};
use crate::instance::Live;
use crate::state::{Kill, StoreState, WallCapExceeded};
use crate::template::TemplateInner;
use crate::{EngineInner, Instance, IsoplaneEngine, Result, Template};

impl IsoplaneEngine {
    /// Map the template copy-on-write and bind `b` as this instance's import
    /// vtable.
    pub fn activate(&self, t: &Template, b: Box<dyn HostBindings>) -> Result<Instance> {
        if t.inner.storage_failed() {
            return Err(crate::TemplateStorageFailed.into());
        }
        self.activate_with(t, Bindings::Runtime(b))
    }

    /// Activate for restricted preparation without host resource authority.
    pub fn activate_initialization(
        &self,
        t: &Template,
        b: Box<dyn InitializationBindings>,
    ) -> Result<Instance> {
        self.activate_with(t, Bindings::Initialization(b))
    }

    fn activate_with(&self, t: &Template, mut bindings: Bindings) -> Result<Instance> {
        bindings.restore_prepared_resources(&t.inner.prepared_resources)?;
        let data = StoreState::with_bindings(bindings, &self.inner.cfg);
        let live = instantiate(&self.inner, &t.inner, data)?;
        Ok(Instance {
            engine: self.inner.clone(),
            template: t.inner.clone(),
            live,
            discarded: None,
            last: None,
            last_violation: None,
        })
    }
}

/// The shared activation path: a `Store`, the hooks, an instantiation, and
/// the export resolution the hot path needs.
pub(crate) fn instantiate(
    engine: &Arc<EngineInner>,
    t: &Arc<TemplateInner>,
    mut data: StoreState,
) -> Result<Live> {
    if t.storage_failed() {
        return Err(crate::TemplateStorageFailed.into());
    }
    let activation = tracing::info_span!(
        "activation",
        linear_memory_bytes = tracing::field::Empty,
        mutable_globals = t.mut_globals.len(),
        tables = t.tables.len(),
    );
    let _activation = activation.enter();
    data.platform_files = t.filesystem_files.clone();
    let mut store =
        tracing::info_span!("activation_store").in_scope(|| Store::new(&engine.wasmtime, data));
    store.limiter(|d| &mut d.limits);

    // The depth behind `is_parked` comes from wasmtime's own transition hook,
    // so it reflects the engine's view of the native stack rather than this
    // crate's bookkeeping around a call.
    store.call_hook(|mut ctx, hook| {
        let d = ctx.data_mut();
        match hook {
            CallHook::CallingWasm => d.wasm_depth += 1,
            CallHook::ReturningFromWasm => d.wasm_depth -= 1,
            _ => {}
        }
        Ok(())
    });

    // Epochs request a best-effort check; the monotonic meter determines usage.
    store.epoch_deadline_callback(|mut ctx| {
        let m = ctx.data().meter;
        ctx.data_mut()
            .check_occupancy(crate::meter::occupancy_nanos()?)?;

        if let Some(w) = m.wall0 {
            let wall = w.elapsed().as_nanos() as u64;
            if wall >= m.slice_wall_cap_ns {
                ctx.data_mut().kill = Some(Kill::Wall);
                return Err(WallCapExceeded { wall_ns: wall }.into());
            }
        }
        Ok(UpdateDeadline::Continue(m.check_ticks))
    });

    // With epoch interruption on, a store starts with an expired deadline.
    store.set_epoch_deadline(u64::MAX / 2);

    let inst = tracing::info_span!("activation_instantiate").in_scope(|| {
        t.pal_linker
            .as_ref()
            .unwrap_or(&engine.linker)
            .instantiate(&mut store, t.module())
            .map_err(anyhow::Error::from)
            .context("activating template")
    })?;

    let exports_stage = tracing::info_span!("activation_exports").entered();
    let mut memory = None;
    let mut f_alloc = None;
    let mut f_handle = None;
    let mut f_wake = None;
    let mut f_gc_hint = None;
    let mut f_initialize = None;
    let mut f_initialize_runtime = None;
    {
        let exports: Vec<(String, Extern)> = inst
            .exports(&mut store)
            .map(|e| (e.name().to_string(), e.into_extern()))
            .collect();
        for (name, ext) in exports {
            match (name.as_str(), ext) {
                (guest_exports::MEMORY, Extern::Memory(m)) => memory = Some(m),
                (guest_exports::ALLOC, Extern::Func(f)) => f_alloc = Some(f),
                (guest_exports::HANDLE, Extern::Func(f)) => f_handle = Some(f),
                (guest_exports::WAKE, Extern::Func(f)) => f_wake = Some(f),
                (guest_exports::GC_HINT, Extern::Func(f)) => f_gc_hint = Some(f),
                (guest_exports::INITIALIZE, Extern::Func(f)) => f_initialize = Some(f),
                (guest_exports::INITIALIZE_RUNTIME, Extern::Func(f)) => {
                    f_initialize_runtime = Some(f)
                }
                _ => {}
            }
        }
    }

    let memory = memory.with_context(|| {
        format!(
            "module does not export `{}` (required)",
            guest_exports::MEMORY
        )
    })?;
    let want = |o: Option<wasmtime::Func>, n: &str| -> Result<wasmtime::Func> {
        o.with_context(|| format!("module does not export `{n}` (required)"))
    };
    let f_alloc = want(f_alloc, guest_exports::ALLOC)?
        .typed::<(i32, i32), i32>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i32,i32)->i32", guest_exports::ALLOC))?;
    let f_handle = want(f_handle, guest_exports::HANDLE)?
        .typed::<(i32, i32), i64>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i32,i32)->i64", guest_exports::HANDLE))?;
    let f_wake = want(f_wake, guest_exports::WAKE)?
        .typed::<(i64, i32, i32), i64>(&store)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("`{}` must be (i64,i32,i32)->i64", guest_exports::WAKE))?;
    let f_gc_hint = match f_gc_hint {
        Some(f) => Some(
            f.typed::<i32, ()>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("`{}` must be (i32)->()", guest_exports::GC_HINT))?,
        ),
        None => None,
    };
    let f_initialize_runtime = match f_initialize_runtime {
        Some(f) => Some(
            f.typed::<(), i64>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| {
                    format!("`{}` must be ()->i64", guest_exports::INITIALIZE_RUNTIME)
                })?,
        ),
        None => None,
    };
    let f_initialize = match f_initialize {
        Some(f) => Some(
            f.typed::<(), i64>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("`{}` must be ()->i64", guest_exports::INITIALIZE))?,
        ),
        None => None,
    };

    let mut globals = Vec::with_capacity(t.mut_globals.len());
    for (name, _) in &t.mut_globals {
        globals.push(
            inst.get_global(&mut store, name)
                .with_context(|| format!("exported mutable global `{name}` vanished"))?,
        );
    }
    let mut tables = Vec::with_capacity(t.tables.len());
    for (name, _) in &t.tables {
        tables.push(
            inst.get_table(&mut store, name)
                .with_context(|| format!("exported table `{name}` vanished"))?,
        );
    }

    #[cfg(target_os = "linux")]
    let mut storage = None;
    #[cfg(target_os = "linux")]
    if let Some(layer) = &t.initialization_layer {
        let _restore = tracing::info_span!("activation_layer_restore").entered();
        let pages = memory.size(&store);
        if u64::from(t.memory_pages) > pages {
            memory.grow(&mut store, u64::from(t.memory_pages) - pages)?;
        }
        // SAFETY: instantiation owns the idle Store, and the verified immutable image
        // matches the captured memory size. No guest entry occurs before all globals restore.
        unsafe { memory.restore_cow_image(&mut store, layer) }?;
        let start = memory.data_ptr(&store) as usize;
        storage = Some(crate::storage_fault::Watch::new(
            start..start + memory.data_size(&store),
            crate::storage_fault::Access::ReadWrite,
        )?);
        restore_globals(&mut store, &globals, t)?;
    }
    // Without a copy-on-write mapping of the layer, activation copies it over
    // the memory the unchanged module instantiated with.
    #[cfg(not(target_os = "linux"))]
    if t.image.is_layer() {
        let _restore = tracing::info_span!("activation_layer_restore").entered();
        let pages = memory.size(&store);
        if u64::from(t.memory_pages) > pages {
            memory.grow(&mut store, u64::from(t.memory_pages) - pages)?;
        }
        t.image.write_to(memory.data_mut(&mut store));
        restore_globals(&mut store, &globals, t)?;
    }
    within_memory_limit(memory.data_size(&store) as u64, &store)?;
    let f_initialize = if t.has_initialize { f_initialize } else { None };
    drop(exports_stage);
    let table_baseline = tracing::info_span!("activation_table_baseline")
        .in_scope(|| crate::capture::TableBaseline::capture(&mut store, &tables))?;
    store.data_mut().memory = Some(memory);
    activation.record("linear_memory_bytes", memory.data_size(&store));

    Ok(Live {
        #[cfg(target_os = "linux")]
        storage,
        store,
        memory,
        f_alloc,
        f_handle,
        f_wake,
        f_gc_hint,
        f_initialize,
        f_initialize_runtime,
        globals,
        tables,
        table_baseline,
    })
}

/// Refuses a template whose memory is already larger than the instance's
/// memory limit. The ceiling above the limit is room for dispatches to finish
/// in, not for the template, so no instance of it could start.
fn within_memory_limit(bytes: u64, store: &Store<StoreState>) -> Result<()> {
    let limit = store.data().limits.memory_limit_bytes as u64;
    anyhow::ensure!(
        bytes <= limit,
        "initialization left {} MiB in memory, more than the application's memory limit of {} MiB",
        bytes >> 20,
        limit >> 20
    );
    Ok(())
}

pub(crate) fn restore_globals(
    store: &mut Store<StoreState>,
    globals: &[wasmtime::Global],
    template: &TemplateInner,
) -> Result<()> {
    for (global, (_, value)) in globals.iter().zip(&template.mut_globals) {
        global.set(&mut *store, wasmtime::Val::from(*value))?;
    }
    Ok(())
}
