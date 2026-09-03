//! Hibernation snapshot support: capturing and restoring an instance's
//! mutable globals and tables against its template's baseline.

use anyhow::{bail, Context};
use wasmtime::{Ref, Val};

use crate::instance::Instance;
use crate::snapshot::GlobalValue;
use crate::{InstanceImage, Result};

pub(crate) fn capture_mutable_state(inst: &mut Instance) -> Result<InstanceImage> {
    if let Some(why) = inst.tombstone {
        bail!(
            "refusing to capture a tombstoned instance ({why}); its memory is a state no \
             execution produced"
        );
    }
    let template = inst.template.clone();
    let live = anyhow::Context::context(inst.live.as_mut(), "instance has no live store")?;

    let mut globals = Vec::with_capacity(live.globals.len());
    for g in &live.globals {
        let v = g.get(&mut live.store);
        globals.push(GlobalValue::of(v, &live.store)?);
    }

    // wasmtime's public API cannot name a funcref by function index, so a
    // table mutation can be detected but not represented in a delta.
    let table_mutations = Vec::new();
    for (i, t) in live.tables.iter().enumerate() {
        let baseline = template
            .table_nullness
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let size = t.size(&live.store);
        let table_name = template
            .tables
            .get(i)
            .map(|(n, _)| n.as_str())
            .unwrap_or("?");
        if size as usize != baseline.len() {
            bail!(
                "table `{table_name}` grew from {} to {size} entries; table mutations cannot \
                 be represented in a delta",
                baseline.len()
            );
        }
        for (slot, was_non_null) in baseline.iter().enumerate() {
            let now_non_null = match t.get(&mut live.store, slot as u64) {
                Some(Ref::Func(f)) => f.is_some(),
                Some(Ref::Extern(e)) => e.is_some(),
                Some(_) => true,
                None => false,
            };
            if now_non_null != *was_non_null {
                bail!(
                    "table `{table_name}` slot {slot} changed since activation; table \
                     mutations cannot be represented in a delta"
                );
            }
        }
    }

    Ok(InstanceImage {
        globals,
        table_mutations,
        memory_pages: live.memory.size(&live.store) as u32,
    })
}

pub(crate) fn restore_mutable_state(inst: &mut Instance, img: &InstanceImage) -> Result<()> {
    if inst.tombstone.is_some() {
        bail!("refusing to restore into a tombstoned instance");
    }
    if !img.table_mutations.is_empty() {
        bail!("table mutations cannot be applied: wasmtime exposes no function-index to funcref mapping");
    }
    let live = anyhow::Context::context(inst.live.as_mut(), "instance has no live store")?;

    // Grow first, so the delta pages `wisp-hibernate` writes through
    // `memory()` have somewhere to land.
    let cur = live.memory.size(&live.store) as u32;
    if img.memory_pages > cur {
        live.memory
            .grow(&mut live.store, (img.memory_pages - cur) as u64)
            .map_err(anyhow::Error::from)
            .context("growing linear memory to the captured size")?;
    }

    if img.globals.len() != live.globals.len() {
        bail!(
            "delta carries {} globals but this template has {}; a delta is only valid \
             against its own module hash and ABI rev",
            img.globals.len(),
            live.globals.len()
        );
    }
    for (g, v) in live.globals.iter().zip(&img.globals) {
        g.set(&mut live.store, Val::from(*v))
            .map_err(anyhow::Error::from)
            .context("restoring a mutable global")?;
    }
    Ok(())
}
