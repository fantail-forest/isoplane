use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Manifest, PreparedError, Result};
use crate::snapshot::GlobalValue;
use crate::template::TemplateInner;
use crate::{MemoryImage, ModuleHash, Template, WispEngine};

#[derive(Serialize, Deserialize)]
pub(super) struct State {
    memory_pages: u32,
    deferred: Vec<u32>,
    mut_globals: Vec<(String, GlobalValue)>,
    tables: Vec<(String, u64)>,
    table_nullness: Vec<Vec<bool>>,
    has_gc_hint: bool,
    has_initialize: bool,
}

impl State {
    pub(super) fn capture(template: &Template) -> Self {
        let inner = &template.inner;
        Self {
            memory_pages: inner.memory_pages,
            deferred: inner.deferred.clone(),
            mut_globals: inner.mut_globals.clone(),
            tables: inner.tables.clone(),
            table_nullness: inner.table_nullness.clone(),
            has_gc_hint: inner.has_gc_hint,
            has_initialize: inner.has_initialize,
        }
    }

    pub(super) fn validate(&self, image: &MemoryImage, max_memory_bytes: u64) -> Result<()> {
        if u64::from(self.memory_pages) * 65536 != image.len() as u64
            || image.len() as u64 > max_memory_bytes
        {
            return Err(PreparedError::Invalid("initial memory dimensions"));
        }
        if self.tables.len() != self.table_nullness.len()
            || self
                .tables
                .iter()
                .zip(&self.table_nullness)
                .any(|((_, len), bits)| *len != bits.len() as u64)
        {
            return Err(PreparedError::Invalid("initial table dimensions"));
        }
        let mut names = HashSet::new();
        for name in self
            .mut_globals
            .iter()
            .map(|(name, _)| name)
            .chain(self.tables.iter().map(|(name, _)| name))
        {
            if name.is_empty() || !names.insert(name) {
                return Err(PreparedError::Invalid(
                    "duplicate or empty state export name",
                ));
            }
        }
        Ok(())
    }
}

impl WispEngine {
    pub(super) fn finish_prepared(
        &self,
        module: wasmtime::Module,
        manifest: Manifest,
        state: State,
        image: MemoryImage,
    ) -> Result<Template> {
        let (pal_linker, pal_composed) = self
            .template_linker(&module)
            .map_err(PreparedError::Engine)?;
        Ok(Template {
            hash: ModuleHash(manifest.module_hash),
            abi_rev: manifest.abi_rev,
            inner: Arc::new(TemplateInner {
                module,
                pal_linker,
                pal_composed,
                image: Arc::new(image),
                memory_pages: state.memory_pages,
                deferred: state.deferred,
                mut_globals: state.mut_globals,
                tables: state.tables,
                table_nullness: state.table_nullness,
                has_gc_hint: state.has_gc_hint,
                has_initialize: state.has_initialize,
            }),
        })
    }
}
