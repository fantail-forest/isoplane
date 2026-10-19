//! Hibernation snapshot support: capturing and restoring an instance's
//! mutable globals and tables against its template's baseline.

use anyhow::{bail, Context};
use wasmtime::{Ref, Store, Table, Val};

use crate::instance::Instance;
use crate::snapshot::GlobalValue;
use crate::{InstanceImage, Result};

pub(crate) fn capture_mutable_state(inst: &mut Instance) -> Result<InstanceImage> {
    if let Some(why) = inst.discarded {
        bail!(
            "refusing to capture a discarded instance ({why}); its memory is a state no \
             execution produced"
        );
    }
    let template = inst.template.clone();
    let live = &mut inst.live;

    let mut globals = Vec::with_capacity(live.globals.len());
    for g in &live.globals {
        let v = g.get(&mut live.store);
        globals.push(GlobalValue::of(v, &live.store)?);
    }

    live.table_baseline
        .verify(&mut live.store, &live.tables, &template)?;

    Ok(InstanceImage {
        globals,
        table_mutations: Vec::new(),
        memory_pages: live.memory.size(&live.store) as u32,
    })
}

pub(crate) fn restore_mutable_state(inst: &mut Instance, img: &InstanceImage) -> Result<()> {
    if inst.discarded.is_some() {
        bail!("refusing to restore into a discarded instance");
    }
    if !img.table_mutations.is_empty() {
        bail!("table mutations cannot be applied: wasmtime exposes no function-index to funcref mapping");
    }
    let live = &mut inst.live;

    // Grow first, so the delta pages `isoplane-hibernate` writes through
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

// These identities are compared only inside the Live store that owns them; they are
// never serialized, dereferenced, or reused by another activation.
#[derive(PartialEq, Eq)]
struct FunctionIdentity(std::num::NonZeroUsize);

#[derive(PartialEq, Eq)]
enum TableEntry {
    Null,
    Function(FunctionIdentity),
}

pub(crate) struct TableBaseline(Vec<Vec<TableEntry>>);

impl TableBaseline {
    pub(crate) fn capture(
        store: &mut Store<crate::state::StoreState>,
        tables: &[Table],
    ) -> Result<Self> {
        let mut baseline = Vec::with_capacity(tables.len());
        for table in tables {
            let mut entries = Vec::with_capacity(table.size(&*store) as usize);
            for index in 0..table.size(&*store) {
                entries.push(table_entry(store, table, index)?);
            }
            baseline.push(entries);
        }
        Ok(Self(baseline))
    }

    pub(crate) fn verify(
        &self,
        store: &mut Store<crate::state::StoreState>,
        tables: &[Table],
        template: &crate::template::TemplateInner,
    ) -> Result<()> {
        for (index, table) in tables.iter().enumerate() {
            let baseline = self
                .0
                .get(index)
                .context("table baseline missing from its live store")?;
            let name = &template.tables[index].0;
            let size = table.size(&*store);
            if size as usize != baseline.len() {
                return Err(crate::snapshot::SnapshotError::TableMutation {
                    table: name.clone(),
                    slot: None,
                }
                .into());
            }
            for (slot, expected) in baseline.iter().enumerate() {
                if table_entry(store, table, slot as u64)? != *expected {
                    return Err(crate::snapshot::SnapshotError::TableMutation {
                        table: name.clone(),
                        slot: Some(slot as u64),
                    }
                    .into());
                }
            }
        }
        Ok(())
    }
}

fn table_entry(
    store: &mut Store<crate::state::StoreState>,
    table: &Table,
    index: u64,
) -> Result<TableEntry> {
    match table
        .get(&mut *store, index)
        .context("live table slot disappeared")?
    {
        Ref::Func(None) => Ok(TableEntry::Null),
        Ref::Func(Some(function)) => Ok(TableEntry::Function(FunctionIdentity(
            std::num::NonZeroUsize::new(function.to_raw(store) as usize)
                .context("a non-null function reference has no identity")?,
        ))),
        _ => bail!("table slot {index} is not a function reference and cannot be captured"),
    }
}
