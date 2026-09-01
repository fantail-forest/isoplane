//! The non-memory half of an instance's mutable universe (invariant 1).
//!
//! Linear memory is diffed by `wisp-hibernate`; globals and tables are small
//! and captured whole.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstanceImage {
    /// Mutable globals, in index order. Immutable globals come from the
    /// template and are re-sealed on activation, so they are not captured.
    pub globals: Vec<GlobalValue>,
    /// Mutations to mutable tables (funcref/externref slots the guest wrote).
    pub table_mutations: Vec<TableMutation>,
    /// Current linear-memory size in WASM pages, so restore can grow to match
    /// before applying delta pages.
    pub memory_pages: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum GlobalValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMutation {
    pub table: u32,
    pub index: u32,
    /// Function index, or `None` for a null slot. Typed function tables mean
    /// there are no arbitrary indirect calls to forge.
    pub func: Option<u32>,
}
