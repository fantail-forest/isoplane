//! Instance activation: mapping a template copy-on-write and wiring the
//! epoch and call hooks that make a dispatch meterable.

use std::sync::Arc;

use anyhow::Context;
use wasmtime::{CallHook, Extern, Store, UpdateDeadline};

use wisp_abi::imports::exports as guest_exports;

use crate::bindings::{Bindings, HostBindings, InitializationBindings};
use crate::instance::Live;
use crate::state::{BudgetExceeded, Kill, StoreState, WallCapExceeded};
use crate::template::TemplateInner;
use crate::{EngineInner, Instance, Result, Template, WispEngine};

impl WispEngine {
    /// Map the template copy-on-write and bind `b` as this instance's import
    /// vtable.
    pub fn activate(&self, t: &Template, b: Box<dyn HostBindings>) -> Result<Instance> {
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

    fn activate_with(&self, t: &Template, bindings: Bindings) -> Result<Instance> {
        let data = StoreState::with_bindings(bindings, &self.inner.cfg);
        let live = instantiate(&self.inner, &t.inner, data)?;
        Ok(Instance {
            engine: self.inner.clone(),
            template: t.inner.clone(),
            live: Some(live),
            tombstone: None,
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
    data: StoreState,
) -> Result<Live> {
    let mut store = Store::new(&engine.wasmtime, data);
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

    // Epochs are wall-clock ticks and cannot express a CPU budget; they give
    // a periodic, guest-preempting callback in which the thread clock is
    // compared. The order of the two comparisons is the two-phase kill: the
    // hard deadline at B+G first and unconditionally, ignoring the
    // unsafe-to-trap bit, so a pinned bit buys exactly the grace window; then
    // grace entry at B, which does not trap and defers while the bit is set,
    // because delivering `Cancelled` into a half-mutated GC is the corruption
    // tombstones exist to contain.
    store.epoch_deadline_callback(|mut ctx| {
        let m = ctx.data().meter;
        let cpu = m.dispatch_cpu_ns(crate::meter::thread_cpu_nanos()?);

        if cpu >= m.hard_ns() {
            ctx.data_mut().kill = Some(Kill::CpuHard);
            return Err(BudgetExceeded {
                cpu_ns: cpu,
                budget_ns: m.cpu_budget_ns,
                grace_ns: m.grace_ns,
            }
            .into());
        }

        // A deferral leaves `grace_at_ns` unset so the next tick tries again.
        if cpu >= m.cpu_budget_ns && ctx.data().grace_at_ns.is_none() {
            if ctx.data().unsafe_to_trap {
                ctx.data_mut().grace_deferrals += 1;
            } else if let Some(mut b) = ctx.data_mut().bindings.take() {
                b.enter_grace();
                let d = ctx.data_mut();
                d.bindings = Some(b);
                d.grace_at_ns = Some(cpu);
            }
        }

        if let Some(w) = m.wall0 {
            let wall = w.elapsed().as_nanos() as u64;
            if wall >= m.wall_cap_ns {
                ctx.data_mut().kill = Some(Kill::Wall);
                return Err(WallCapExceeded { wall_ns: wall }.into());
            }
        }
        Ok(UpdateDeadline::Continue(m.check_ticks))
    });

    // With epoch interruption on, a store starts with an expired deadline.
    store.set_epoch_deadline(u64::MAX / 2);

    let inst = t
        .pal_linker
        .as_ref()
        .unwrap_or(&engine.linker)
        .instantiate(&mut store, &t.module)
        .map_err(anyhow::Error::from)
        .context("activating template")?;

    let mut memory = None;
    let mut f_alloc = None;
    let mut f_handle = None;
    let mut f_wake = None;
    let mut f_gc_hint = None;
    let mut f_initialize = None;
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
    let f_initialize = match f_initialize {
        Some(f) => Some(
            f.typed::<(), ()>(&store)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("`{}` must be ()->()", guest_exports::INITIALIZE))?,
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

    let table_baseline = crate::capture::TableBaseline::capture(&mut store, &tables)?;
    store.data_mut().memory = Some(memory);

    Ok(Live {
        store,
        memory,
        f_alloc,
        f_handle,
        f_wake,
        f_gc_hint,
        f_initialize,
        globals,
        tables,
        table_baseline,
    })
}
